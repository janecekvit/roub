# CLAUDE.md

Guidance for Claude Code (claude.ai/code) when working in this repository.

## Project overview

roub is a server-centric version control system for agent-scale monorepos, an independent
implementation of the design Anthropic described for Scion. Clients are always online and fetch
only what they need; the server creates commits, merges, rebases and answers history queries.
One Rust codebase runs on Linux (Debian), Windows and Azure.

Status: early. `roub-core` implements the object format and tree derivation; roub-server serves
every RPC from PostgreSQL and a local blob directory (or from memory), without authentication or
TLS yet. Cloud blob stores, the cache, the daemon and the CLI are still to come. `docs/design/decisions.md` lists what is decided
and what is still open.

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo test -p roub-core path::          # one module
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check                        # advisories, licences, sources
scripts/render-diagrams.sh              # format and render docs/**/*.d2 to SVG
scripts/render-diagrams.sh --check      # what CI runs: formatted, valid, SVG up to date
cd fuzz && cargo +nightly-2026-10-01 fuzz run decode_changeset   # needs cargo-fuzz
cargo test -p roub-server --test rpc    # end-to-end gRPC tests; PostgreSQL ones need the variable below
ROUB_TEST_DATABASE_URL=postgres://roub:roub@localhost/roub_test cargo test -p roub-server
cargo run -p roub-server -- --listen 127.0.0.1:7470 --repository monorepo   # in memory
ROUB_DATABASE_URL=postgres://… ROUB_BLOB_DIR=/var/lib/roub/blobs cargo run -p roub-server -- --repository monorepo
docker build -t roub-server .           # Debian image
docker compose up                       # roub-server on PostgreSQL (+ Azurite, not used yet)
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on Ubuntu and Windows, coverage
(cargo-llvm-cov, uploaded to Codecov; the only job with a PostgreSQL service, so the PostgreSQL
tests run there), cargo-deny (workspace and `fuzz/`), short fuzz runs of the
decoders on a pinned nightly, a Docker build and the diagram check.
`security.yml` runs CodeQL (Rust and the workflows) and zizmor; `scorecard.yml` runs OpenSSF
Scorecard. The toolchain is pinned in `rust-toolchain.toml`; keep the CI workflow and the
Dockerfile on the same version.

## Layout

| Crate | Binary | Role |
|---|---|---|
| `roub-core` | | Pure logic, no I/O: object ids, canonical encoding of trees and changesets, tree derivation, path, repository and ref name rules. Shared by every tier. |
| `roub-proto` | | gRPC contract in `proto/roub/v1/roub.proto`, compiled with a vendored `protoc`. |
| `roub-cli` | `roub` | Short-lived client; talks only to `roubd`. |
| `roubd` | `roubd` | Node daemon: one object store per node, file watcher, dirty-path set. |
| `roub-cache` | `roub-cache` | Regional cache: trees and blobs from RAM, upstream only on a miss. |
| `roub-server` | `roub-server` | Authoritative, stateless API fleet over PostgreSQL and the blob store. The RPC handlers (`service.rs`) validate everything and run on any `Store`: `PostgresStore` (schema in `migrations/`, blobs in `blob.rs`) or `MemoryStore`. |

Shared versions, lints and package metadata live in the root `Cargo.toml`
(`[workspace.package]`, `[workspace.dependencies]`, `[workspace.lints]`). Add dependencies there
and reference them with `.workspace = true`.

## Architecture rules

These come from the Scion design and are what makes roub scale. Do not break them without
updating `docs/design/decisions.md`.

- **Strict tiering.** `roub` → `roubd` → `roub-cache` → `roub-server`. A tier talks only to the
  next one. `ObjectService` RPCs are identical at every tier so clients never know the topology.
- **Objects are immutable and content-addressed.** Ids are BLAKE3-256 of the canonical,
  uncompressed encoding with a per-kind derive-key context (`roub-core::object`). The context
  strings are part of the format: never edit them. `docs/design/object-format.md` specifies the
  encoding; change it only with a new format version and context string, and update the spec,
  the golden tests and the fuzz targets in the same change.
- **Trees are derived.** A changeset lists only `Put`/`Delete` changes relative to its first
  parent and never names a root tree; `roub-core::derive` computes the trees.
- **Refs are the only mutable state and are never cached.** `ResolveRef` and
  `CompareAndSwapRef` always reach roub-server and PostgreSQL. A ref's version only grows, also
  across delete and re-create, and every move is appended to an immutable ref log.
- **Commits take no lock.** `CreateChangeset` appends to the WAL; moving a ref is a separate
  compare-and-swap on one row.
- **The server computes every id.** Clients send blob contents and changeset fields; the server
  hashes them and never stores an object under an id it did not compute. `CreateChangeset` is
  idempotent, so a retry is safe.
- **Cost follows the change.** Nothing on the commit path may scan the whole repository.
- **Paths are validated at commit time** with `roub-core::path::RepoPath` and
  `find_case_collisions`, so every checkout works on Linux and Windows.
- **The server owns authorization.** Per-directory ACLs are enforced at roub-server; how caches
  avoid becoming an ACL bypass is an open decision.

## Conventions

- `unsafe` is denied workspace-wide. FFI for ProjFS or FUSE goes in a small dedicated module with
  `#[allow(unsafe_code)]` and a safety comment on every block.
- Libraries return typed errors (`thiserror`); binaries use `anyhow`.
- Changing the wire contract: edit `roub.proto`, keep field numbers stable, add fields rather than
  reuse numbers, and update every tier that serves the RPC.
- Platform differences (virtual file system, watcher, local IPC, service host) go behind traits
  with one implementation per OS, selected with `cfg`.
- Tests for pure logic live next to the code in `#[cfg(test)]` modules; property tests that
  cross modules live in the crate's `tests/` directory.
- Schema changes are new files in `crates/roub-server/migrations/`, never edits to an applied one.
  Objects, refs and the ref log are protected by triggers; keep it that way.
- End-to-end tests in `crates/roub-server/tests/rpc.rs` are listed in `store_tests!`, which runs
  each one against every `Store`.
- The repository is public: never put links to Claude sessions (`claude.ai/code/session_…`) or
  other private URLs in commits, pull request descriptions, comments or files.
- Pull requests are squash-merged with the title and description as the commit message, so write
  the description as that message: what changes and why, plus how it was tested. Instructions
  for the reviewer or for repository settings go in a PR comment, not the description.

## Diagrams

- **D2 is the source of truth for architecture.** `docs/architecture/roub-architecture.d2` holds
  the tiers, boundaries and numbered flows. Read it before changing how tiers talk to each other.
  When a component, boundary or flow changes, edit the `.d2`, run `scripts/render-diagrams.sh` and
  commit the regenerated `.svg` in the same change. Install D2 with
  `go install oss.terrastruct.com/d2@v0.7.2`, the build CI uses: the SVG embeds the D2 version.
- **Mermaid is for flows over time.** Sequence diagrams (checkout, commit) live inline in
  `docs/architecture/README.md`, where GitHub renders them. Update them when an RPC or its order
  changes.
- No draw.io, no ASCII art for architecture.

## CI and supply chain

roub is open source and serves code across trust boundaries, so its own supply chain is held to
the same bar as the product. See `SECURITY.md` for reporting.

- **Pin every action to a full commit SHA** with the release in a comment
  (`uses: owner/action@<sha> # v1.2.3`). Dependabot keeps both up to date.
- **New versions wait 14 days.** Dependabot's `cooldown` does this for updates; apply the same
  rule by hand to any action, crate or tool version you add.
- **Least privilege:** workflows default to `contents: read`; a job that needs more (`id-token`,
  `security-events`) asks for it at job level. Check out with `persist-credentials: false`.
- **No stored secrets:** Codecov uploads through GitHub OIDC.
- **Coverage is informational** (`codecov.yml`): it is reported on every pull request but does
  not block it. New logic in `roub-core` still comes with tests.
- **New dependencies** must pass `cargo deny check`: allowed licences, no RustSec advisories,
  crates.io only.
- Run `zizmor .github/workflows` (and `actionlint`) after editing a workflow.
- GitHub Copilot code review follows `.github/copilot-instructions.md` (it reads only the
  first 4,000 characters). When a rule here changes, update that file too.

## License

Apache-2.0 (`LICENSE`, `NOTICE`). Every crate inherits `license = "Apache-2.0"` from the workspace.

# Architecture

- **Tiers and boundaries:** [`roub-architecture.d2`](roub-architecture.d2) is the source of truth.
  It is written in [D2](https://d2lang.com) and rendered to
  [`roub-architecture.svg`](roub-architecture.svg) by `scripts/render-diagrams.sh`. CI fails when
  the SVG is out of date, the source is not formatted, or it does not compile.
- **Flows over time:** the Mermaid sequence diagrams below render directly on GitHub.

Update the D2 source and re-render in the same change whenever a component, boundary or flow
changes; update the sequence diagrams when an RPC or its order changes.

## Tiers

![roub architecture](roub-architecture.svg)

Blue edges are a checkout (1–6), orange edges a commit (7–11), grey dashed edges supporting paths.
Dashed containers are failure domains and deployment boundaries.

## Checkout

```mermaid
sequenceDiagram
    autonumber
    participant CLI as roub (CLI)
    participant D as roubd
    participant C as roub-cache
    participant S as roub-server
    participant M as PostgreSQL
    participant B as Blob store

    CLI->>D: checkout main --sparse src/
    D->>C: ResolveRef(main)
    C->>S: ResolveRef (passes through, never cached)
    S->>M: read the ref row
    S-->>D: changeset id
    loop root tree down, only directories in the sparse profile
        D->>D: which trees and blobs are missing locally?
        D->>C: GetObjects(missing ids)
        alt cache hit
            C-->>D: objects from RAM
        else cache miss
            C->>S: GetObjects(missing ids)
            S->>B: read by content address
            S-->>C: objects
            C-->>D: objects
        end
    end
    D->>D: write files, or serve them on demand through FUSE / ProjFS
    D-->>CLI: checked out
```

## Commit

```mermaid
sequenceDiagram
    autonumber
    participant CLI as roub (CLI)
    participant D as roubd
    participant C as roub-cache
    participant S as roub-server
    participant B as Blob store
    participant M as PostgreSQL

    Note over D: the file watcher already holds the dirty-path set
    CLI->>D: commit -m "…"
    D->>C: PutBlobs(new blobs)
    C->>S: PutBlobs (passes through)
    S->>B: put-if-absent per blob
    S-->>D: stored / already present
    D->>C: CreateChangeset(parent, changed paths, blob ids)
    C->>S: CreateChangeset (passes through)
    S->>S: validate paths (case collisions, Windows names)
    S->>M: append the changeset to the WAL, no lock
    S-->>D: changeset id
    D->>C: CompareAndSwapRef(main, expected, desired)
    C->>S: CompareAndSwapRef (passes through)
    alt main still points at expected
        S->>M: UPDATE refs … WHERE changeset = expected
        S-->>D: swapped
    else another change landed first
        S-->>D: not swapped, current main
        D->>S: rebase onto current main (server-side), then retry the swap
    end
    D-->>CLI: committed
```

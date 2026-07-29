# Blitz collaborative docs

A Google Docs-inspired rich-text editor backed by a small Convex-style realtime sync engine running on Blitz.

## Features

- rich-text editing with headings, lists, quotes, code, and inline formatting
- canonical Markdown storage with a source editing mode
- WebSocket query subscription and mutation protocol
- optimistic edits with server-assigned revisions
- automatic rebasing for concurrent edits to different text ranges
- explicit conflict recovery for overlapping edits
- collaborator presence, reconnect, save state, and an in-memory document store

## Run

From this directory, launch the Blitz API and Vite together through Turborepo:

```sh
bun run dev
```

Open `http://localhost:5173` in two browser windows to see realtime editing and presence. Turbo supervises both persistent tasks; `bun run dev:api` and `bun run dev:web` remain available when you need to run one side alone.

## Protocol

Clients subscribe with `{ type: "subscribe", documentId, clientId, name }`. The server returns a complete snapshot and then broadcasts revisioned patches. Mutations contain one contiguous text change and the revision on which it was authored. The server transforms stale, non-overlapping changes across its bounded operation history. Changes touching the same range return a conflict plus the authoritative snapshot.

This is intentionally a first vertical slice: state and presence are in-memory, identity is anonymous, and overlapping edits use explicit conflict recovery rather than a full CRDT.

# Blitz for Zed

The Zed extension attaches the Blitz language server to JavaScript, TypeScript,
and TSX buffers while retaining Zed's built-in grammars and syntax queries.

For local development:

1. Run `bun install` in `language-server/`.
2. Build Blitz once with Cargo, or set `BLITZ_COMPILER` to a Blitz executable.
3. In Zed, run **Zed: Install Dev Extension** and select `editors/zed/`.

When editing the Blitz repository, the extension starts
`language-server/src/lsp.ts` with Bun. In other worktrees it expects a
`blitz-language-server` executable on `PATH`; run `bun run build && bun link`
from `language-server/` to provide it.

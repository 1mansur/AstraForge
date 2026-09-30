# AstraForge
A local-first engineering workbench built with Tauri 2, Rust, React, TypeScript, Monaco, SQLite, Tree-sitter and Git.

This is an initial engineering implementation, not a claim that all production acceptance criteria have been met. Features operate on real repositories and processes. There is no simulated terminal, repository dataset, AI response or browser filesystem bridge in the shipped application.

Windows installer, source archive and SHA-256 checksums are available in [GitHub Releases](https://github.com/1mansur/AstraForge/releases/tag/v0.1.0). The installer is unsigned.

## Run
Install Node.js 22.12 or later, pnpm 11.19, Git, stable Rust with the Windows MSVC target, Visual Studio C++ Build Tools and WebView2. See the [official Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

```powershell
pnpm install --frozen-lockfile
pnpm desktop
```

The browser development URL deliberately reports that desktop access is required. Filesystem access is provided only through the Tauri host. Use **Open repository** and enter a local Git repository's absolute path.

```powershell
pnpm typecheck
pnpm lint
pnpm test
cargo test -p astraforge-core
cargo clippy -p astraforge-core --all-targets -- -D warnings
cargo fmt --all -- --check
pnpm build
pnpm package
```

`pnpm package` builds the Windows NSIS installer. Native outputs are under `target/release/bundle/nsis`. Signing credentials are not included; production distribution requires your signing and release process.

## What works
- Lazy repository exploration, UTF-8 editing in Monaco, conflict-aware saves and filesystem changes.
- AST symbol and reference extraction for TypeScript, JavaScript, Rust, Python and JSON; structural Markdown parsing; file dependency traversal.
- Incremental persistent indexing, literal/regex/symbol/filename search, local cosine vector search with an optional embedding provider and lexical fallback.
- Git status, staged and unstaged diffs, history, branch creation/checkout, staging, unstaging and commits.
- Multiple real piped command sessions with stdin, separate stdout/stderr, bounded output, exit codes and process-tree cancellation.
- Persisted hash-checked patch proposals, per-file review, apply/reject/revert, durable journals and startup recovery.
- A configurable streaming HTTP AI provider adapter, typed tools, public execution traces, scoped memory, explicit approvals and bounded repair attempts.
- Light/dark/system themes, command palette, configurable shortcuts, repository health and local diagnostics.

The command panel is a piped process console, not a full ConPTY terminal emulator. Programs requiring terminal escape-sequence emulation or interactive console APIs need an external terminal. AI features require a configured compatible provider; all local engineering features work without one.

## Example repository workflow
1. Open a Git repository and let indexing complete. Expand folders and open a source file.
2. Edit and save with Ctrl/Cmd+S. Saving verifies the original content hash and refuses conflicting external edits. Clean editor buffers refresh when an external change is reported.
3. Search text or symbols; open a result. Use the graph panel to inspect dependencies and references.
4. Inspect the Git diff, stage selected files and commit with a message.
5. Set a test executable and argument list in Settings, then run tests. Review the displayed command and approve it. Stop a process from its session when needed.

## Example agent workflow
1. Configure an HTTPS provider base URL, model and API-key environment-variable name. A loopback HTTP service is also supported. Set the environment variable before starting AstraForge. No API key is saved in the database.
2. Start a focused task, such as “Inspect the token refresh implementation and propose a regression fix.” Repository and tool content are labeled as untrusted data.
3. Follow the public plan, tools and observations. Review the proposed diff in Changes and apply or reject it.
4. Continue the session. Review and approve the exact proposed test command. Failed results feed back into the next bounded repair iteration.
5. Stop a running session, export the versioned session JSON, or reopen a persisted session after restart. Imported sessions are inspection-only and cannot execute tools.

Configure an embedding model only if you want repository snippets sent to that provider during repository indexing. Opening a repository through the Open repository dialog automatically starts indexing; the Build index action also starts it. Without embeddings, semantic search falls back to lexical search. The adapter speaks a configurable Chat Completions/SSE and embeddings protocol; providers with different protocols require another trait implementation.

## Engineering documentation
- [Architecture and extension boundaries](docs/architecture.md)
- [Event and request protocol](docs/protocol.md)
- [Security and threat model](docs/security.md)
- [Testing and validation](docs/testing.md)
- [Performance measurements](docs/performance.md)
- [Engineering audit and known limitations](docs/engineering-report.md)
- [Generated database schema](docs/schema.sql)
- [Generated AI tool schema](docs/ai-tools.schema.json)

The database lives in the operating system's app-data directory for `dev.astraforge.workbench`. Keep that directory private: repository snippets, patch snapshots and tool output can contain sensitive data. External requests occur through configured AI operations and, when an embedding model is configured, repository indexing. Local editing, Git, lexical indexing/search and process execution do not require an AI provider.

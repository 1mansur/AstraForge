# Engineering report
## Delivery status
AstraForge is a working initial desktop implementation with real repository, filesystem, SQLite, Git, process, indexing, patch and configurable AI-provider integrations. It is not a claim that every item in the requested forty-phase production specification is complete. No simulated repository data, AI responses or terminal output are shipped.

## Architecture and implementation
The frontend separates typed bridge services, domain stores/controllers and React views. The Tauri host dispatches work away from the UI thread. A standalone Rust core owns capability-rooted filesystem access, persistence, AST indexing, graph/search, Git, processes, patch transactions, providers, vectors and agent orchestration.

Implemented workflows include repository discovery; real UTF-8 editing and hash-checked saves; incremental indexing and filesystem invalidation; exact, regex, filename and symbol search; name-based references and dependency graph traversal; Git status/history/branches/staging/commits/diffs; command/test stdin and streamed output; reviewable per-file patches with approval/rejection/revert/recovery; streaming configurable AI tasks/reviews; scoped memory; persisted execution graphs and tool logs; bounded repair; versioned session export/import; health, themes and keyboard commands.

Local vectors persist by repository/provider/file hash, support batching, prefix filtering and incremental replacement/deletion, and degrade to lexical search. Common credential-file paths are excluded from agent context and embedding generation. Exported sessions include recorded tool calls, referenced patch snapshots, command/test results and final state; imported archives are inspection-only.

## Security decisions
Repository contents and tool output never define agent policy. Provider tools cannot supply approval flags, directly apply a patch or mutate files through a filesystem API. Patch application remains a separate user action. Command approval is bound to the exact frozen executable/arguments/environment/test flag, and repository captures remain stable when an agent resumes.

Applied, reverted and partially accepted agent patches require a successful actual test run before the agent can complete. Failed tests retain the verification gate. The repair loop is bounded by configured steps and three patch proposals. Windows child processes start suspended, enter a kill-on-close Job object, and resume only after ownership is established.

Filesystem controls reject traversal, symlinks, junctions, Git metadata, Windows device aliases and alternate data streams. SQLite transactions, app-instance locking and a durable patch journal detect incomplete work and avoid silently overwriting recovery conflicts. See [the security review](security-audit.md) for concrete findings and fixes.

## Measured performance
The optimized synthetic benchmark indexed 1,000 / 10,000 / 50,000 small TypeScript files in 1.54 / 11.05 / 59.32 seconds. At 50,000 files, warm exact-text search median was 1.88 ms, symbol substring search 78.44 ms, and a single-file update 447.57 ms. These are actual development-host measurements, not production performance guarantees. Resident memory is a snapshot, not peak memory; fixture generation and cleanup are excluded from indexing. See [the complete benchmark report](performance.md).

## Validation
The final command logs are in [validation](validation/). The Rust suite passed 59 tests with zero failures; two subprocess helpers and a separately executed performance test are intentionally ignored in the ordinary run. Rust tests exercise real temporary repositories and processes, including the full repository → failing test → reviewed patch → passing test → Git diff → revert → restart workflow. HTTP loopback fixtures exercise actual streaming and embedding transport; scripted AI responses exist only inside orchestration tests.

The frontend has 32 passing tests across four files: 12 controller tests, 6 command-parsing tests, 3 shortcut tests and 11 React component tests. Full TypeScript checking includes dependency declarations, without `skipLibCheck`; ESLint is clean and the production Vite build succeeds. Monaco is loaded separately from the shell. The remaining large-chunk warning is recorded rather than suppressed. The [frontend validation record](validation/frontend-validation.txt) distinguishes these automated checks from native visual verification.

Windows GNU debug and optimized release native builds were successfully produced with a workspace-local Rust/C toolchain. The unsigned Windows x64 NSIS installer was built successfully. The final executable stayed alive for an eight-second startup smoke test, started WebView2, initialized SQLite, and closed gracefully without stderr output. This verifies process startup, not native UI workflows. Artifact hashes, packaging and test results are recorded in [delivery-status.json](delivery-status.json). No paid or authenticated live AI-provider request was made. Browser visual verification could not be completed because the local browser surface repeatedly timed out/reset; no UI screenshot is presented as verified.

## Known limitations
- This is a pipe-based command console, not a ConPTY/PTY terminal emulator. Full interactive console applications need an external terminal. Shell detection and cross-platform process behavior need broader validation.
- The command policy is not an OS sandbox. Explicitly approved scripts execute with the user's privileges and may modify files. Repository path capabilities constrain application file APIs, not arbitrary child-process code.
- Reference/caller results are syntactic name matches, not compiler-resolved identities. Module aliases, macros, dynamic imports and changes to previously unresolved dependency targets need richer language-server integration.
- Test selection currently uses the user-configured executable/arguments. Automatic dependency-based test selection and structured framework-specific failure extraction are not implemented.
- Git has file-level staging, not interactive hunk staging. There is no debugger adapter, breakpoint execution or advanced merge/conflict editor. Git cancellation is timeout/process-tree based rather than a complete user-cancellable operation API.
- The exact vector store is linear in stored vectors and embeds bounded file snippets. It is not an ANN index or a complete semantic model of every line. Semantic search displays up to 100 results, including its lexical fallback, and does not offer additional pages; text, regex, symbol and filename searches retain pagination. Large symbol substring search remains a measured optimization target.
- Provider cancellation is checked between network frames; a stalled blocking request can take up to its 60-second timeout to release. Only the compatible Chat Completions/SSE/embeddings protocol is implemented; other provider protocols require adapters.
- Secret-path guards are not a complete content-based secret scanner. Source code, logs and commit messages can contain secrets. The local SQLite database and exports are not encrypted.
- External writers can race the final hash-check/replacement window. Multi-file filesystem operations are journaled, not one atomic OS transaction. Reverting deletion restores content with ordinary creation permissions; original ACLs, extended metadata and executable bits are not fully journaled.
- Session archives are capped at 8 MiB. Import preserves historical artifacts without granting execution authority. Unsaved editor buffers have close protection but are not a full crash-restored editor session.
- Unresolved external graph imports have no local file destination and are displayed without a navigation action. Git lists, editor tabs and agent session lists need broader large-workspace UI stress testing.
- Fuzz targets are included but sustained fuzz campaigns were not run. Native GUI end-to-end automation, accessibility testing, actual-provider evaluation, installer signing and controlled mixed-language/peak-memory benchmarks remain release work.

## Recommended next engineering steps
1. Exercise the packaged application manually and through native Windows UI automation on clean machines, including a configured provider and installer upgrade/uninstall.
2. Add ConPTY, an OS execution sandbox, framework-aware test selection and compiler/language-server reference resolution.
3. Improve symbol search using measured workloads, add graph invalidation for newly resolved targets, and benchmark real repositories with concurrent editing and agent activity.
4. Run sustained fuzzing, external security review, signed-release CI and migration/upgrade tests before declaring the product production-ready.

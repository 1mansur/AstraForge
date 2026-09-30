# AstraForge 0.2.0 engineering report

Phase 2 hardens the existing Tauri/Rust/React application while preserving its architecture and real filesystem, Git, SQLite, process and provider integrations. This is an adversarially tested development release, not a claim of production readiness. [Delivery metadata](delivery-status.json) records final results and artifact hashes; detailed evidence is under [phase2](phase2/).

## Baseline

The unchanged initial commit `5e8d89382507c0473a1948790eb6a3244c1472d7` passed 59 Rust tests and 32 frontend tests before hardening edits. Three Rust entries were intentionally ignored: two subprocess fixtures and a separately run performance test. Clippy with warnings denied, formatting, TypeScript, ESLint, source-style constraints and the production web build also passed. The first frontend sandbox attempt could not load Vite/esbuild because of an ancestor-directory permission restriction; unchanged source passed with the required host access. Both attempts, the original report and delivery metadata are preserved in [baseline](phase2/baseline/).

## Reproduced failures and corrections

| Subsystem | Trigger and prior failure | Root cause and correction |
| --- | --- | --- |
| Editor | Concurrent reads erased drafts or discarded newer disk state; old repository replies populated the next repository. | Stale snapshots were treated as current. Generation checks, single-flight reads, revision checks and mutation/switch guards preserve drafts and repository ownership. |
| Watcher/UI | 1,000 events caused 3,000 immediate IPC calls; a 10,000-line diff mounted 10,000 rows. | Coalescing reduces immediate requests to three; virtualization mounts 36 rows in the measured viewport. A deferred-event scrolling crash was also reproduced and fixed. |
| Native watcher | Ignored temporary-file events filled the queue; atomic saves emitted parent-directory modifications that caused broad subtree rescans. | Filter ignored/access events before queue admission and discard existing-directory content/metadata modifications while retaining directory create/remove/rename and explicit rescan flags. Two regressions failed before the correction and pass afterward. |
| Arguments/output | Settings rewrote Windows paths/empty arguments; tiny output fragments accumulated excessive rendering work. | Argument display round-trips the parsed vector; retained chunks and sessions are bounded separately from byte limits. |
| Workspace/Git | `core.worktree` redirected a selected repository to a sibling/ancestor; changing it after open redirected Git operations. | Discovery binds the canonical root to the nearest actual `.git` marker; later Git calls pin the captured work tree. Normal nested folders and linked worktrees remain supported. |
| Git execution | A clean filter executed during automatic diff; a `git.cmd` wrapper or repository-local executable entered automatic paths. | Automatic entry points validate native executables, exclude repository-local resolution and use a minimal environment. Built-in Git calls disable clean/smudge/process drivers, hooks, fsmonitor and external diff. |
| SQLite/patches | Rejecting a future schema still changed journal mode; Unicode lowercasing falsely merged distinct Windows patch paths. | Version preflight precedes persistent pragmas; native ordinal comparison replaces Unicode lowercasing. Crash/rollback tests verify external changes survive and incomplete journals remain recoverable. |
| Dependencies | Unchanged importers retained stale targets after create/delete/rename; discovery preceded the index gate. | Persist imports/candidate paths, reconcile affected edges and acquire the cancellable gate before discovery. Windows matching uses ordinal collation and canonical target spelling. |
| Incremental SQL | The 50,000-file watcher test exceeded its 30-second bound; per-path reconciliation and candidate lookup each scanned the repository repeatedly. | Use indexed equality/prefix-range queries and a covering ordinal candidate index. A real update of 100 paths among 50,000 cached paths/candidates falls from exceeding five million VM instructions before each correction to 757,000 afterward. Unicode/adjacent-folder and existing-index migration regressions also pass. |
| Positions/Markdown | UTF-8-byte/scalar positions disagreed with Monaco UTF-16 columns; dense headings repeatedly scanned source prefixes. | Emit UTF-16 columns, migrate analysis versions, precompute Markdown line offsets and cap parser items. |
| Search/vectors | Cancellation missed queued SQL; cancelled or stale embedding responses overwrote fresh state. | Register scoped cancellation before admission; propagate it through SQL progress handlers, transport and vector scans. Commits recheck cancellation, indexed hashes and dimensions. |
| Processes | Non-reading stdin blocked an RPC; completion held its output lock while waiting for SQLite. | A bounded input queue isolates writes; persistence occurs outside the output lock. Termination is idempotent and monitor/watcher workers are joined appropriately. |
| Agent persistence | One malformed history row blocked startup; failed import left an orphan session; stop rewrote terminal history. | Stream/validate startup rows, quarantine invalid records transactionally, commit session/archive together and enforce state transitions. |
| Agent lifecycle | Panics consumed worker slots permanently; a late save transiently resurrected a cancelled session. | Fallible worker creation and unwind cleanup release slots. Conditional atomic upsert prevents non-cancelled state from replacing a cancelled record. |
| Provider | EOF/truncation counted as successful completion; duplicate SSE IDs duplicated text; vector dimensions differed. | Bounded incremental parsing validates completion and event identity; embedding batches validate dimensions. Null/empty tool fields remain compatible; actual unsupported calls fail. |
| Provider cancellation | Stalled body reads ignored cancellation; dropping per-request runtimes could still wait for DNS. | A shared bounded async runtime and four retained resolver leases let callers cancel without waiting for synchronous OS DNS teardown or accumulating an unbounded resolver queue. |
| Desktop/control | Queued work could use another selected repository; diagnostic insertion failure could prevent stop. | Versioned envelopes capture ownership, events carry identity/sequence, command/session IDs are checked, and stop/cancel controls bypass optional operation logging. |

Detailed reproductions, exact tests, before/after logs and residual risks are in the [frontend](phase2/frontend-audit.md), [storage](phase2/storage-audit.md), [engine](phase2/engines-audit.md), [agent](phase2/agent-audit.md) and [provider/service](phase2/provider-service-audit.md) audits. Late review findings are distinguished from failures observed against the unchanged baseline; no exact baseline failure is claimed for an in-flight compilation.

## Security and concurrency

Provider/repository text remains untrusted data. Patch application and command approval remain separate explicit actions. File capabilities, original hashes, journals and guarded rollback prevent stale application writes. Six real process exits cover journal/final-commit boundaries for apply and revert; real SQL aborts exercise rollback and atomic import/quarantine. Conflicting recovery opens for inspection while blocking mutation until the user resolves files and reopens the repository.

A separate real child process exits during the first active index transaction, after file/FTS/symbol writes and before batch commit. Reopening preserves the previous seven-table snapshot and completed generation, marks the unfinished run interrupted and passes SQLite, foreign-key, FTS and orphan checks. Reindexing edited, renamed, deleted and newly created files converges to an independent clean index and every persisted disk hash. This verifies the chosen transaction boundary, not whole-repository atomicity across all indexing batches.

Desktop requests reject incompatible versions, missing scope and stale repository IDs before mutation and retain the captured Workspace throughout dispatch. Watchers carry a stop token and recheck generation. The frontend reconciles sequence gaps and bounds refresh work. Index/search cancellation survives database-admission waits; stop remains usable when operation-log writes fail.

Built-in Git filter disabling also bypasses Git LFS/custom clean-filter transformations. Driver discovery and invocation are separate processes; same-user concurrent configuration replacement is not strongly isolated. Approved scripts retain normal account privileges. These controls are not an OS sandbox, code signing or protection against arbitrary same-user malware.

## Integrated hostile workflow

The [automated workflow](phase2/hostile-workflow.md) combines more than 1,000 real indexed files, concurrent rename/delete/edit/search/Git activity, six loopback HTTP requests, three patch proposals and an actual failing child test. It rejects a stale patch after an external edit, applies a fresh patch, exercises bounded repair, cancels stalled SSE, restarts Engine and compares every remaining indexed hash with disk.

Assertions cover SQLite integrity/foreign keys, renamed/deleted paths, orphan index rows, patch journals, session state, command/operation completion, Git diff and child PID termination. The standalone recorded run cancelled the stream in 17 ms and completed in 38.97 seconds under shared host load. This is real core integration, not a claim of clicking through the native UI or testing a paid remote model.

## Final validation

The ordinary Rust suite passes **132 tests**, with zero failures and ten intentional ignored entries: nine subprocess helpers invoked by parent tests and one separately executed patch benchmark. The frontend passes **74 tests across nine files**. Clippy with warnings denied, formatting, TypeScript, ESLint, source style, schema export and production web build pass. The separate 100-file patch measurement recorded proposal/apply/revert times of 183/299/278 ms and a 16 ms SQLite patch-list query; these were debug-profile measurements under shared host load.

`pnpm package` passed in 328.94 seconds and built the Windows x64 NSIS installer. The final bundled executable remained alive for an eight-second startup observation, completed real renderer-to-Rust `repositories` and `settings` requests, emitted no stdout/stderr and closed gracefully. The first completed operation was observed 865 ms after launch; this is not a first-frame rendering measurement. Rust-host peak working set was 30.31 MiB, excluding its observed WebView child and GPU processes. The binary hash and observations are recorded in [native-startup.json](phase2/final/native-startup.json). Installer install/upgrade/uninstall and a full native GUI workflow remain untested.

New Rust protocol/stream properties execute 4,500 generated cases, alongside existing path, patch, parser and vector properties. These checks are distinct from coverage-guided fuzzing. A real local libFuzzer build failed in its Windows-specific C++ source with GNU; the failure is retained. A Linux GitHub Actions workflow is configured for four instrumented targets with time/resource limits. Public publication is authorized; the configured Linux run is pending upload. Its verified outcome will be recorded in delivery metadata. Native packaging/startup results are recorded in delivery metadata. [Final command records](phase2/final/) include commands, exit codes and elapsed times.

## Performance

The [Phase 2 performance report](phase2/performance.md) uses separate optimized processes for mixed 1,000/10,000/50,000-file fixtures. It records component startup, full/incremental indexing, warm query distributions, cancellation, database/WAL growth, process output and OS peak resident memory. Backend peak includes fixture generation; it is not native GUI/WebView/GPU peak. Deterministic 16-dimensional embeddings measure mechanics, not model quality or network latency. The [Phase 1 report](performance.md) uses a different synthetic corpus and is historical, not a controlled before/after comparison.

| Source files | Backend startup (ms) | Full index (s) | One-file update (ms) | Warm text p95 (ms) | 100-write watcher convergence (s) | Peak backend RSS (MiB) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 | 694.72 | 1.36 | 289.56 | 0.12 | 1.57 | 17.37 |
| 10,000 | 339.64 | 14.55 | 278.56 | 0.37 | 1.51 | 18.58 |
| 50,000 | 202.45 | 54.12 | 312.92 | 0.61 | 1.94 | 29.87 |

The final 50,000-file database/WAL/shared-memory checkpoint totals 201.62 MiB. Its real child process produces 16 MiB across stdout/stderr and completes in 101.93 ms; bounded-buffer truncation is explicit, so this is not a lossless terminal-throughput claim. These local warm-cache observations are not production service-level guarantees.

Measured jsdom improvements include 3,000 to three immediate event-storm requests and 10,000 to 36 mounted diff rows. The same 10,000-line diff fixture changed from 348.91 to 88.51 ms mount time in the focused comparison. These are component-test observations, not native frame-rate guarantees.

## Architectural changes and limitations

Changes remain within existing modules: versioned envelopes, scoped cancellation/generations, bounded stream/DNS handling, durable quarantine/operation records, dependency caches/analysis versions and bounded UI/process queues. Existing synchronous provider traits remain. No fake provider, alternative database or simulated terminal was shipped.

- Full native GUI automation, live authenticated providers, installer install/upgrade/uninstall and installer signing remain distinct from core/component tests and startup smoke checks.
- Finite fuzzing does not prove absence of vulnerabilities; independent external security review remains outstanding.
- The complete cancellation/provider-failure/abrupt-restart matrix across every persistent agent and approval state has not been exercised. The report distinguishes real process exits, synthesized recovery records and orderly Engine restarts.
- OS DNS cannot be forcibly aborted; at most four leases may remain occupied until it returns. Destructive resource exhaustion was not forced on the host.
- The console uses pipes rather than ConPTY; approved commands have normal account privileges.
- Reference/module analysis remains syntactic and heuristic. Compiler/LSP semantics, macros, aliases and automatic affected-test selection remain unimplemented.
- Vector search remains linear and the semantic UI caps at 100 results. Larger real monorepos, cold-disk behavior and sustained load remain unverified.
- SQLite serializes access. Cancellation is cooperative around synchronous parser/filesystem work; multi-file replacement is recoverable but not one OS transaction.
- The Windows notify backend can lose events on native buffer overflow without exposing a rescan flag. Application-queue overflow and reported rescan signals are handled; watching is not guaranteed lossless, and an explicit full reindex may be needed after extreme churn.
- Histories, quarantine, snippets and exports are not encrypted. There is no new quarantine repair UI or backup service.
- The large lazy Monaco chunk warning remains visible. Component timing is explicitly labelled jsdom.

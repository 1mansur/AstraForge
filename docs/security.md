# Security model and threat model
## Trust boundaries
Trusted policy is backend code and explicit user configuration/approval. A user task is a separate instruction layer. Repository files, Git commit messages, tool output, stored repository facts and generated model content are untrusted data. The provider is an untrusted proposer, never a filesystem or process authority. The model prompt labels these layers explicitly; runtime validation and separate approval channels enforce the important effects even when model behavior is wrong.

## Assets and adversaries
Assets are repository contents, Git state, credentials, process environment, local database and the user's account. Adversaries include malicious repository contributors, compromised scripts/dependencies, prompt-injected files, a compromised provider and accidental concurrent editor writes. Arbitrary same-user malware is outside the protection boundary.

| Threat | Implemented controls | Residual limit |
| --- | --- | --- |
| Path traversal / symlinks / junctions | Capability-rooted I/O, nofollow checks, Windows alias/device/ADS rejection, metadata exclusion | A privileged or same-user process can still attack the application; command processes are not capability-confined |
| Stale or manipulated patches | Exact original hashes, multi-file prevalidation, durable journal, guarded rollback/recovery | An unrelated editor can race the final check and replacement; multi-file operations cannot be a single OS transaction |
| Command injection | Separate executable and argument arrays, strict read-only grammar, no shell concatenation, literal Git pathspecs | Explicitly approved scripts and shells have the user's OS privileges |
| Malicious Git hooks/helpers | Disabled hooks, fsmonitor, pager, external diff/textconv and configured clean/smudge/process filters; native executable validation and minimal child environment | Git itself remains trusted; built-in operations bypass Git LFS/custom filter transformations; same-user concurrent configuration replacement is not strongly isolated |
| Repository-root redirection | Bind discovery to the nearest actual Git marker; pin subsequent Git work-tree arguments to the captured root | Bare repositories and custom layouts without a normal working-tree marker are unsupported; linked worktrees remain supported |
| Secret environment exposure | Child environment allowlist; provider key resolved only in backend from a named environment variable | Common credential filenames are excluded from AI tools/embeddings; other source files, test output and patches can still contain secrets |
| Prompt injection | Data labels, typed schema, no provider approval field, backend command policy, explicit patch review | No prompt can guarantee a model ignores every malicious instruction; review requested actions |
| Provider transport attack | HTTPS or loopback HTTP, redirects disabled, bounded frames/responses and timeouts | Local HTTP trusts the local service; provider receives the selected context |
| Resource exhaustion | File/patch/stream limits, batched indexer, bounded queues, graph caps, process/session caps, step/repair limits | Regex and exact vector search are still workload-sensitive; cold large indexing is substantial |
| Crash / DB inconsistency | SQLite WAL/transactions, future-version rejection, incomplete-operation markers, patch journal recovery | No encrypted database or automatic off-device backup; hardware faults can still lose data |
| Concurrent app instances | Exclusive database-lifetime file lock | Separately configured data directories and external tools can target the same repository |

## Local storage and privacy
No external telemetry is configured. The database stores repository snippets, embeddings, patch before/after content, session traces and command output. It relies on OS app-data access controls and is not encrypted. Exported sessions also require care before sharing. Normal SQLite deletion is not a secure-erasure guarantee.

Only configuring an AI endpoint does not silently run an agent. Starting an AI task sends retrieved context to that endpoint. With an embedding model configured, indexing sends bounded document snippets for embeddings. Opening a repository through the dialog automatically starts indexing, and the Build index action also starts it. An empty provider configuration preserves all offline operations.

## Approval and cancellation semantics
Classification is intent/argument based where practical; it is not an OS sandbox. The first unknown build/test program and each agent command requiring approval must be reviewed. Approvals are bound to the concrete proposed command. A model cannot grant itself approval. Patch creation only records a proposal; application is a distinct explicit UI action.

Stop propagates to workers and process trees. Asynchronous provider request/header/body waits poll cancellation every 20 ms. The shared runtime has two async workers and at most four blocking DNS workers; four resolver leases prevent cancelled DNS requests from accumulating an unbounded queue. A synchronous OS DNS call cannot be forcibly aborted and can retain its lease until the OS returns, while the cancelled request returns promptly. Connect and total request limits remain 10 and 60 seconds. A cancelled UI session does not mean every OS cleanup operation is instantaneous. Index/search requests register cancellation before database admission, and stop/cancel control commands bypass optional operation-log writes.

## Release requirements
Before treating this as a hardened production product, add a real OS execution sandbox for untrusted build/test scripts, credential storage through the platform keychain if desired, protocol compatibility testing, signed installers, native GUI automation, sustained fuzzing and an external security review. See [the Phase 2 report](engineering-report.md) and its subsystem audits for current findings, and [the Phase 1 audit](security-audit.md) for historical evidence.

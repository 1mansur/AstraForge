# Event and request protocol
The only desktop domain command is `request`. Its input is `{ "request": { "method": "read_file", "params": { "path": "src/main.ts" } } }`. Methods are snake_case; arguments and results use camelCase. Unknown fields are rejected at the typed Rust request boundary. The frontend Methods type maps each method to its input and output. Errors contain code, message, category, recoverable, cause and context. Sensitive filesystem/database internals are not returned verbatim.

| Domain | Methods |
| --- | --- |
| Repository/files | repositories, open_repository, tree, read_file, write_file, create_file, remove_file, rename_file |
| Index/search | index_repository, cancel_index, search, cancel_search, graph, health |
| Git | git_status, git_diff, git_history, git_branches, git_action |
| Patches | propose_patch, patches, apply_patch, revert_patch, reject_patch |
| Commands | classify_command, start_command, poll_command, command_input, stop_command |
| Agent | start_agent, agent_session, agent_sessions, stop_agent, continue_agent, approve_agent_command, export_session, import_session |
| Configuration | settings, save_settings, diagnostics, tool_schema |

A Workspace is selected in the native host. File arguments are relative paths, never arbitrary absolute paths. `write_file` requires the hash returned by `read_file`. Patch proposals include the original hash, or null for creation. Null resulting content means deletion. Applying and reverting are explicit UI commands; provider tools cannot invoke them directly.

`workspace_changed` events contain `{paths: string[], rescan: boolean}`. Paths are coalesced from native events. A rescan flag means the bounded queue overflowed or a watcher error requires reconciliation. The frontend refreshes affected expanded folders and detects open-file conflicts. With `rescan: true`, it refreshes all expanded folders, open documents, Git status and health even when no concrete paths are present. Dirty buffers remain intact; disk hashes distinguish an unchanged buffer from a conflict. Events are invalidation notices, not filesystem authority.

`diagnostic` events contain `{message: string}` and report failures such as unavailable embeddings. Diagnostics are local and never sent as telemetry.

Commands use `{program, args, cwd, env, approved, isTest}`. Classification occurs in Rust. The agent tool schema excludes the approved field. `start_command` returns an ID. `poll_command` accepts `{id,cursor}` and returns `{id,chunks,nextCursor,exitCode,running,truncated}`. Each chunk contains sequence, stream and text. Consumers advance the cursor; truncation is explicit. Rings cap at 1 MiB per process; each poll caps around 128 KiB. At most 16 processes run and 128 sessions are retained in memory.

Agent start returns a persisted session immediately, while a native worker executes. Session polling returns graph nodes and source/target edges, public summaries, pending approvals and status. Node details are bounded. Completion, failure, cancellation, approval waits, import and crash interruption are explicit states. `continue_agent` never approves a pending command; `approve_agent_command` is the separate trust transition. Imported JSON is versioned and inspection-only.

The current protocol has no cross-version network transport promise. Add a protocol version before exposing it outside the single bundled desktop application. Generated tool JSON Schema and SQLite DDL are in this directory.

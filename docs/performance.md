# Performance measurements

This document preserves the historical Phase 1 measurements and interpretation. Its tables and numbers describe the original uniform TypeScript fixture, not the current mixed-language harness. See [Phase 2 performance](phase2/performance.md) for the current methodology and results; the two fixtures are not a controlled before/after comparison.

Measured on 2026-09-30 on Windows 10.0.26200, x86_64, using an optimized Rust build. These are actual executions against generated Git repositories and SQLite databases. The complete machine-readable results are in [benchmarks.json](benchmarks.json). CPU model and installed RAM were unavailable through this host's restricted system inventory. Other build activity was present during the run, so these measurements are a development baseline rather than controlled hardware comparisons.

## Repository operations

| Files | Fixture creation (s) | Index (s) | Text p50 / p95 (ms) | Symbol p50 / p95 (ms) | One-file update (ms) | Resident memory after (MiB) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1000 | 0.24 | 1.54 | 0.60 / 1.72 | 0.31 / 0.71 | 174.78 | 10.86 |
| 10000 | 48.55 | 11.05 | 1.78 / 2.65 | 6.72 / 11.15 | 123.98 | 12.14 |
| 50000 | 283.63 | 59.32 | 1.88 / 2.42 | 78.44 / 109.53 | 447.57 | 12.11 |

Every repository contains the stated number of TypeScript files, with one exported interface, one function, and one dependency per file. Indexing produced 2,000 / 20,000 / 100,000 symbols and exactly 1,000 / 10,000 / 50,000 dependencies. The one-file update reparsed exactly one file in all three cases. Fixture creation and temporary-directory cleanup are excluded from indexing duration.

## Database, graph, patches, and output

| Files | Direct graph p50 (ms) | SQLite count p50 (ms) | Patch proposal (ms) | Patch apply (ms) | 8 MiB output command (ms) | Main database (MiB) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1000 | 0.07 | 0.05 | 1.27 | 18.93 | 182.13 | 4.11 |
| 10000 | 0.55 | 0.43 | 0.74 | 17.42 | 110.80 | 40.22 |
| 50000 | 13.42 | 23.31 | 1.11 | 51.78 | 475.26 | 202.54 |

The command-streaming fixture writes exactly 8,388,608 bytes through a real child process. The manager retains at most 1 MiB and returns at most 128 KiB per poll. Every run explicitly reported truncation because the producer outpaced the polling consumer. These timings include process startup, actual pipe streaming, completion, and local persistence; they are not lossless terminal transport measurements. Integration tests separately verify stdin, cancellation, and descendant process termination.

## Method and interpretation

Text, symbol, direct graph, and SQLite timings use 20 warm samples. The reported median is the upper middle sample; p95 is the nineteenth ordered sample. Text queries target one exact substring near the end of the repository. Symbol queries search for the final generated function. The graph measurement queries immediate dependents, not whole-repository transitive closure. The SQLite measurement counts repository symbols. Patch measurements create and transactionally apply one real single-file modification.
The text index uses case-sensitive SQLite FTS5 trigrams to select candidate files, followed by exact matching to recover line and column. A development run exposed a query-plan regression in an ordinary FTS join at 10,000 files: the repository index could be scanned before repeatedly evaluating the FTS expression. An explicit FTS-first CROSS JOIN removed that plan choice. Final text-search median stayed below 2 ms through 50,000 files on this fixture.
Symbol substring matching still scans persisted symbol names. At 100,000 symbols it reached 78.44 ms median and 109.53 ms p95; a dedicated symbol prefix or trigram projection is the next measured optimization. Direct graph queries and symbol-count queries also became slower once the persisted working set grew beyond the small SQLite page cache. Real application cache sizing should be evaluated with concurrent editing and indexing before changing durability or cache policies.
The 50,000-file fixture required 283.63 seconds to create on this Windows filesystem, while its actual index took 59.32 seconds. Fixture creation is therefore a material harness cost, not indexer latency. Incremental timing includes scoped Git enumeration, safe process startup, a content hash, the changed AST, and a transactional database update. Parsing is single-worker with 64-file transaction batches; concurrency does not grow with file count. Individual files are limited to 2 MiB, parsing has a two-second budget, and extracted AST items are capped at 50,000 per file.
Resident-memory values are process working-set snapshots before and after indexing, not peak allocation measurements. All sizes execute sequentially in one process, so allocator reuse affects growth values. Main database size excludes the WAL, shared-memory file, and temporary Git repository. The synthetic fixture uses small files and does not establish memory bounds for repositories full of maximum-size files.
Regex and literal queries shorter than three characters use bounded scanning of persisted content. They can return an explicit SEARCH_BUDGET error after 128 MiB or five seconds and should not be described as sublinear searches. Cancellation checks occur between files and every 256 source lines. AST references are name-based and do not perform compiler-level type or scope resolution.

## Reproduce

Run the following from the repository root with a supported Rust toolchain, C compiler, Git, and Windows PowerShell available:

```sh
cargo run -p astraforge-core --release --example performance -- 1000 10000 50000
```
The example prints progress and completed per-size measurements to stderr, then emits a single versioned JSON report to stdout. It cleans up its own generated repositories on successful completion. Benchmark the packaged UI, mixed-language real repositories, cold-cache search, deep graph traversal, peak memory, watcher bursts, and concurrent agent activity before using these figures as production targets.

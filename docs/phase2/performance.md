# Phase 2 engine performance
Measurements use `crates/core/examples/phase2_performance.rs` in the optimized Rust release profile. Each scale runs in a separate process with a new disposable Git repository and SQLite database. Fixture generation is timed separately from indexing. The final artifact is `benchmarks.json`; individual run JSON and stderr logs preserve the raw results.
The fixture mixes TypeScript, Python, Rust, JSON, Markdown, and JavaScript across nested directories with Unicode and spaces. It includes syntax errors, repeated symbol names, a 983,040-byte text file, one file just above the two MiB read cap, an ignored source file, and a real `.gitignore`. Source-file counts exclude those control files; exactly source count plus two files must be indexed.
Warm text, symbol, regex, transitive graph, and exact vector searches use twenty samples; the upper-middle observation is reported as median, and the nineteenth sorted observation is p95. Cancellation measures request-to-worker-return time. Search cancellation reports whether the worker observed it, so a request that finishes before cancellation is not represented as a cancelled operation. The watcher measurement writes one hundred files and waits until every persisted content hash matches its final filesystem content and all started callbacks finish, with an unchanged thirty-second convergence bound. The process measurement generates sixteen MiB across real stdout and stderr, polls through completion, and reports buffer truncation explicitly.
Embeddings come from a deterministic local sixteen-dimensional fixture for every eligible indexed document. This measures persistence and cosine scan costs. It does not measure network latency, provider throughput, or semantic relevance.
Peak memory is the operating system's `PeakWorkingSet64` for the benchmark process on Windows, including fixture generation and all later operations. It is not a before/after difference, the desktop UI peak, child-process peak, GPU memory, or total machine usage. Backend startup measures database open, repository validation, and index initialization, not graphical application launch. SQLite main file, WAL, and shared memory are measured separately at two checkpoints. These checkpoints do not claim peak disk usage.
The machine reports Intel Core 5 210H, twelve logical processors, 16,780,787,712 physical-memory bytes, AMD64, and Windows NT 10.0.26200. Hardware was read from the registry and `GlobalMemoryStatusEx` because CIM access was denied. Exact capture metadata is in `engines-hardware.json`. The benchmark is a local warm-cache run, not a cold-disk or cross-machine comparison. Other build and test jobs are paused during the measurement window.
Phase 1 numbers in `docs/benchmarks.json` remain a baseline for their original uniform TypeScript fixture. They are not a controlled before/after comparison with this mixed fixture.

Reproduce from the repository root with `cargo build -p astraforge-core --release --example phase2_performance`, then run the resulting executable separately with arguments `1000`, `10000`, and `50000`. On this Windows GNU toolchain, the build used `RUSTFLAGS=-C link-self-contained=yes`; the artifact hash records the exact executable measured.

## Final measurements
All three scales completed on the final watcher implementation. Exact measurements, provenance hashes, hardware, run timestamps, and limitations are in [benchmarks.json](benchmarks.json). Values below are rounded only for readability.

| Metric |1,000 source files|10,000 source files|50,000 source files|
|---|---:|---:|---:|
|Initial index, seconds|1.36|14.55|54.12|
|Fixture generation, seconds|0.42|30.49|187.60|
|Backend startup, ms|694.72|339.64|202.45|
|Peak process RSS, MiB|17.37|18.58|29.87|
|One-file incremental update, ms|289.56|278.56|312.92|
|100-write watcher convergence, ms|1573.45|1511.96|1935.16|
|100-write producer, ms|1247.03|1008.13|1577.91|
|Index cancellation, ms|113.19|116.06|109.45|
|Search cancellation, ms|0.17|0.02|0.04|
|Local embedding fixture ingestion, ms|46.37|487.91|6840.88|
|16 MiB process stream completion, ms|107.59|130.92|101.93|
|Final database + WAL + SHM, MiB|11.40|48.20|201.62|

Warm query times are median / p95 in milliseconds.

| Query |1,000 source files|10,000 source files|50,000 source files|
|---|---:|---:|---:|
|Literal text|0.09 / 0.12|0.10 / 0.37|0.47 / 0.61|
|Symbol|0.24 / 0.39|3.88 / 4.50|19.57 / 25.61|
|Regex, early 50 hits|0.61 / 1.07|0.65 / 1.15|0.67 / 1.10|
|Transitive dependency graph|0.24 / 0.26|0.26 / 0.36|0.25 / 0.28|
|Exact 16D vector scan|2.97 / 3.30|27.43 / 29.99|122.07 / 190.91|

Every run indexed exactly the source count plus `.gitignore` and the eligible large text file. Both index and search cancellation were observed at every scale. Every process-stream run exercised bounded-buffer truncation, so the reported bytes polled are deliberately less than the sixteen MiB produced. The run does not claim lossless process history.

The first pre-fix 10,000-file run failed the watcher deadline. A later 50,000-file run exposed repeated repository-wide SQL scans after the watcher filtering fix. Both failed runs and the diagnostic repeat remain preserved separately, with the investigations and regressions in [engines-audit.md](engines-audit.md#watcher-benchmark-investigation). Final numbers above exclude those diagnostic runs. Native notification overflow remains a documented dependency limitation; successful convergence of this workload is not a guarantee for arbitrary event storms.

In this run, the one-file update stayed between 279 and 313 ms and the hundred-write watcher workload stayed between 1.51 and 1.94 seconds across the three scales. Initial indexing still took 54.12 seconds at fifty thousand source files. Warm literal-search p95 stayed below one millisecond, while symbol and exact-vector scans grew with the corpus; the fifty-thousand-file vector p95 was 190.91 ms. These are single-machine observations, not guaranteed latency limits or a controlled before/after speed comparison.

The graph query follows the TypeScript import chain inside the first five-hundred-file directory group. It does not measure traversal of a fifty-thousand-node connected graph. The early-hit regex sample likewise does not characterize a full-corpus negative regex scan. Peak backend memory and warm query timings do not substitute for desktop UI profiling.

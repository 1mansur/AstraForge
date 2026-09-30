use astraforge_core::database::Database;
use astraforge_core::error::{AppError, Result};
use astraforge_core::index::IndexService;
use astraforge_core::patch::{PatchProposal, PatchService};
use astraforge_core::process::{CommandSpec, ProcessManager};
use astraforge_core::workspace::Workspace;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
fn duration_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn sample(mut operation: impl FnMut() -> Result<()>) -> Result<Value> {
    let mut durations = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        operation()?;
        durations.push(duration_ms(start));
    }
    durations.sort_by(f64::total_cmp);
    Ok(json!({"medianMs":durations[10],"p95Ms":durations[18],"samples":durations.len()}))
}
#[cfg(windows)]
fn memory_bytes() -> Result<u64> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(Get-Process -Id {}).WorkingSet64", std::process::id()),
        ])
        .output()?;
    if !output.status.success() {
        return Err(AppError::new("BENCH_MEMORY", "Windows memory query failed"));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|error| AppError::new("BENCH_MEMORY", error.to_string()))
}
#[cfg(unix)]
fn memory_bytes() -> Result<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or_else(|| AppError::new("BENCH_MEMORY", "RSS measurement is unavailable"))?
        .parse::<u64>()
        .map(|value| value * 1024)
        .map_err(|error| AppError::new("BENCH_MEMORY", error.to_string()))
}
fn benchmark(files: usize) -> Result<Value> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("repository");
    std::fs::create_dir(&root)?;
    let init = Command::new("git")
        .args(["init", "-q"])
        .arg(&root)
        .output()?;
    if !init.status.success() {
        return Err(AppError::new("BENCH_GIT", "Git init failed"));
    }
    let fixture_start = Instant::now();
    for index in 0..files {
        let group = index / 500;
        let directory = root.join(format!("group{group:03}"));
        if index % 500 == 0 {
            std::fs::create_dir(&directory)?;
        }
        let previous = index.saturating_sub(1);
        let previous_group = previous / 500;
        let content = format!("import {{ function{previous} }} from '../group{previous_group:03}/file{previous:06}';\nexport interface Entity{index} {{ id: number; name: string; }}\nexport function function{index}(value: Entity{index}): string {{\n  return String(function{previous}(value)) + 'search-token-{index}';\n}}\n");
        std::fs::write(directory.join(format!("file{index:06}.ts")), content)?;
    }
    let fixture_ms = duration_ms(fixture_start);
    let db = Arc::new(Database::open(&temporary.path().join("benchmark.sqlite"))?);
    let workspace = Workspace::open(
        root.to_str()
            .ok_or_else(|| AppError::new("BENCH_PATH", "Path is not UTF-8"))?,
        &db,
    )?;
    let index = IndexService::new(db.clone())?;
    let cancel = AtomicBool::new(false);
    let memory_before = memory_bytes()?;
    let start = Instant::now();
    let indexed = index.index(&workspace, &cancel)?;
    let index_ms = duration_ms(start);
    let memory_after = memory_bytes()?;
    let text = sample(|| {
        index.search(
            &workspace,
            &format!("search-token-{}'", files - 1),
            "text",
            0,
            50,
        )?;
        Ok(())
    })?;
    let symbols = sample(|| {
        index.search(
            &workspace,
            &format!("function{}", files - 1),
            "symbol",
            0,
            50,
        )?;
        Ok(())
    })?;
    let graph = sample(|| {
        index.graph(&workspace, "group000/file000000.ts", "dependents")?;
        Ok(())
    })?;
    let sqlite = sample(|| {
        db.with(|connection| {
            let _: i64 = connection.query_row(
                "SELECT count(*) FROM index_symbols WHERE repository_id=?1",
                [&workspace.id],
                |row| row.get(0),
            )?;
            Ok(())
        })
    })?;
    let path = "group000/file000000.ts";
    let original = workspace.read(path)?;
    let updated = format!(
        "{}\nexport const incrementallyAdded = true;\n",
        original.content
    );
    workspace.write(path, &updated, &original.hash)?;
    let start = Instant::now();
    let incremental = index.update(&workspace, &[path.to_owned()], &cancel)?;
    let incremental_ms = duration_ms(start);
    let patch_service = PatchService::new(db.clone());
    let current = workspace.read(path)?;
    let start = Instant::now();
    let patch = patch_service.propose(
        &workspace,
        vec![PatchProposal {
            path: path.to_owned(),
            content: Some(format!("{}\nexport const patched = 1;\n", current.content)),
            expected_hash: Some(current.hash),
        }],
        "benchmark",
    )?;
    let patch_generation_ms = duration_ms(start);
    let start = Instant::now();
    patch_service.apply(&workspace, &patch.id)?;
    let patch_application_ms = duration_ms(start);
    let manager = ProcessManager::new(db.clone());
    let start = Instant::now();
    let command = manager.start(
        &workspace,
        CommandSpec {
            program: std::env::current_exe()?.to_string_lossy().to_string(),
            args: vec!["--stream-fixture".to_owned()],
            cwd: None,
            env: BTreeMap::new(),
            approved: true,
            is_test: false,
        },
    )?;
    let mut streamed_bytes = 0;
    let mut cursor = 0;
    let mut stream_truncated = false;
    loop {
        let snapshot = manager.poll(&command, cursor)?;
        streamed_bytes += snapshot
            .chunks
            .iter()
            .map(|chunk| chunk.text.len())
            .sum::<usize>();
        stream_truncated |= snapshot.truncated;
        cursor = snapshot.next_cursor;
        if !snapshot.running {
            if snapshot.exit_code != Some(0) {
                return Err(AppError::new(
                    "BENCH_COMMAND",
                    "Git streaming benchmark failed",
                ));
            }
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            manager.stop(&command)?;
            return Err(AppError::new("BENCH_COMMAND", "Command timed out"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let terminal_ms = duration_ms(start);
    let database_bytes = std::fs::metadata(temporary.path().join("benchmark.sqlite"))?.len();
    let result = json!({"files":files,"fixtureGenerationMs":fixture_ms,"indexMs":index_ms,"indexedFiles":indexed.files,"symbols":indexed.symbols,"dependencies":indexed.dependencies,"residentBytesBefore":memory_before,"residentBytesAfter":memory_after,"residentGrowthBytes":memory_after.saturating_sub(memory_before),"databaseBytes":database_bytes,"textSearch":text,"symbolSearch":symbols,"directGraphTraversal":graph,"sqliteCountQuery":sqlite,"incrementalUpdateMs":incremental_ms,"incrementalFiles":incremental.files,"patchGenerationMs":patch_generation_ms,"patchApplicationMs":patch_application_ms,"terminalCommandMs":terminal_ms,"terminalGeneratedBytes":8 * 1024 * 1024,"terminalPolledBytes":streamed_bytes,"terminalTruncated":stream_truncated});
    drop(manager);
    drop(patch_service);
    drop(index);
    drop(workspace);
    drop(db);
    temporary.close()?;
    Ok(result)
}
fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments == ["--stream-fixture"] {
        let mut output = std::io::stdout().lock();
        let chunk = vec![b'x'; 8192];
        for _ in 0..1024 {
            output.write_all(&chunk)?;
        }
        return Ok(());
    }
    let sizes = if arguments.is_empty() {
        vec![1000, 10_000, 50_000]
    } else {
        arguments
            .iter()
            .map(|value| {
                value
                    .parse::<usize>()
                    .map_err(|error| AppError::new("BENCH_SIZE", error.to_string()))
            })
            .collect::<Result<Vec<_>>>()?
    };
    if sizes.iter().any(|size| *size == 0 || *size > 100_000) {
        return Err(AppError::new(
            "BENCH_SIZE",
            "Use repository sizes between 1 and 100000",
        ));
    }
    let mut results = Vec::new();
    for size in sizes {
        eprintln!("Benchmarking {size} files");
        let result = benchmark(size)?;
        eprintln!("Measurement: {}", serde_json::to_string(&result)?);
        results.push(result);
    }
    let report = json!({"schemaVersion":1,"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"profile":if cfg!(debug_assertions) { "debug" } else { "release" },"methodology":"Generated TypeScript modules; 20 warm search/query samples; process resident memory, not peak; fixture generation excluded from index duration; SQLite FULL synchronous WAL; local filesystem and Git CLI","results":results});
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

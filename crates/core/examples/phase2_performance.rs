use astraforge_core::database::Database;
use astraforge_core::error::{AppError, Result};
use astraforge_core::index::IndexService;
use astraforge_core::process::{CommandSpec, ProcessManager};
use astraforge_core::provider::EmbeddingProvider;
use astraforge_core::vector::VectorStore;
use astraforge_core::watcher;
use astraforge_core::workspace::Workspace;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
fn millis(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn sample(mut operation: impl FnMut() -> Result<()>) -> Result<Value> {
    let mut values = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        operation()?;
        values.push(millis(start));
    }
    values.sort_by(f64::total_cmp);
    Ok(json!({"medianMs":values[10],"p95Ms":values[18],"samples":20}))
}
#[cfg(windows)]
fn memory(peak: bool) -> Result<u64> {
    let field = if peak {
        "PeakWorkingSet64"
    } else {
        "WorkingSet64"
    };
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(Get-Process -Id {}).{field}", std::process::id()),
        ])
        .output()?;
    if !output.status.success() {
        return Err(AppError::new(
            "BENCH_MEMORY",
            "Operating system memory query failed",
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|error: std::num::ParseIntError| AppError::new("BENCH_MEMORY", error.to_string()))
}
#[cfg(unix)]
fn memory(peak: bool) -> Result<u64> {
    let source = std::fs::read_to_string("/proc/self/status")?;
    let field = if peak { "VmHWM:" } else { "VmRSS:" };
    source
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| AppError::new("BENCH_MEMORY", "RSS data missing"))?
        .parse::<u64>()
        .map(|value| value * 1024)
        .map_err(|error| AppError::new("BENCH_MEMORY", error.to_string()))
}
fn database_size(path: &Path) -> Value {
    let main = std::fs::metadata(path)
        .map(|entry| entry.len())
        .unwrap_or(0);
    let wal = std::fs::metadata(format!("{}-wal", path.display()))
        .map(|entry| entry.len())
        .unwrap_or(0);
    let shared = std::fs::metadata(format!("{}-shm", path.display()))
        .map(|entry| entry.len())
        .unwrap_or(0);
    json!({"mainBytes":main,"walBytes":wal,"sharedMemoryBytes":shared,"totalBytes":main+wal+shared})
}
fn source(index: usize) -> (&'static str, String) {
    let item = format!("item{index}");
    let needle = format!("needle-{index}");
    match index%6 {
        0=>("ts",format!("import {{item{}}} from './file{:06}';\nexport function {item}(){{return '{needle} 😀';}}\n",index+6,index+6)),
        1=>("py",format!("def {item}():\n    return '{needle} 😀'\n")),
        2=>("rs",format!("pub fn {item}() -> &'static str {{ \"{needle} 😀\" }}\n")),
        3=>("json",format!("{{\"{item}\":\"{needle} 😀\",\"nested\":{{\"duplicate\":true}}}}\n")),
        4=>("md",format!("# {item}\n{needle} 😀\n[Related](./file000000.ts)\n")),
        _=>("js",format!("export function {item}(){{return '{needle} 😀';}}\n")),
    }
}
struct LocalEmbeddingFixture;
impl EmbeddingProvider for LocalEmbeddingFixture {
    fn identity(&self) -> String {
        "benchmark-local-fixture-16-dimensions".into()
    }
    fn embed(&self, inputs: &[String], cancel: &AtomicBool) -> Result<Vec<Vec<f32>>> {
        if cancel.load(Ordering::Acquire) {
            return Err(AppError::new(
                "cancelled",
                "Local embedding fixture cancelled",
            ));
        }
        Ok(inputs
            .iter()
            .map(|input| {
                let mut vector = vec![0.0; 16];
                for (at, byte) in input.bytes().enumerate() {
                    vector[at % 16] += f32::from(byte) / 255.0;
                }
                vector
            })
            .collect())
    }
}
fn benchmark(files: usize) -> Result<Value> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("mixed repository");
    std::fs::create_dir(&root)?;
    let git = Command::new("git")
        .args(["init", "-q"])
        .arg(&root)
        .output()?;
    if !git.status.success() {
        return Err(AppError::new("BENCH_GIT", "Git init failed"));
    }
    let start = Instant::now();
    let mut paths = Vec::new();
    for number in 0..files {
        let group = number / 500;
        let directory = if group % 7 == 0 {
            format!("group{group:03} Юникод space/deep/nested")
        } else {
            format!("group{group:03}/deep/nested")
        };
        if number % 500 == 0 {
            std::fs::create_dir_all(root.join(&directory))?;
        }
        let (extension, mut text) = source(number);
        if number % 97 == 0 && matches!(extension, "ts" | "js" | "rs") {
            text.push_str("\nfunction incomplete( {\n");
        }
        let path = format!("{directory}/file{number:06}.{extension}");
        std::fs::write(root.join(&path), text)?;
        paths.push(path);
    }
    std::fs::write(root.join("large.txt"), "large text line\n".repeat(65536))?;
    std::fs::write(root.join("oversized.txt"), vec![b'x'; 2 * 1024 * 1024 + 1])?;
    std::fs::write(root.join(".gitignore"), "ignored/\n")?;
    std::fs::create_dir(root.join("ignored"))?;
    std::fs::write(root.join("ignored/cache.ts"), "export const ignored=true;")?;
    let fixture_ms = millis(start);
    eprintln!("Generated {files} mixed source files in {fixture_ms:.1} ms");
    let startup_start = Instant::now();
    let database_path = temporary.path().join("stress.sqlite");
    let db = Arc::new(Database::open(&database_path)?);
    let workspace = Workspace::open(
        root.to_str()
            .ok_or_else(|| AppError::new("BENCH_PATH", "Non-UTF8 fixture root"))?,
        &db,
    )?;
    let index = Arc::new(IndexService::new(db.clone())?);
    let startup_ms = millis(startup_start);
    let resident_before = memory(false)?;
    let start = Instant::now();
    let indexed = index.index(&workspace, &AtomicBool::new(false))?;
    let index_ms = millis(start);
    if indexed.files != files + 2 {
        return Err(AppError::new(
            "BENCH_INDEX",
            format!(
                "Expected {} indexed fixture files, got {}",
                files + 2,
                indexed.files
            ),
        ));
    }
    eprintln!("Indexed {files} files in {index_ms:.1} ms");
    let database_after_index = database_size(&database_path);
    let text = sample(|| {
        let hits = index.search(&workspace, &format!("needle-{} ", files - 1), "text", 0, 50)?;
        if hits.is_empty() {
            return Err(AppError::new("BENCH_SEARCH", "Expected text hit missing"));
        }
        Ok(())
    })?;
    let symbol = sample(|| {
        index.search(&workspace, &format!("item{}", files - 1), "symbol", 0, 50)?;
        Ok(())
    })?;
    let regex = sample(|| {
        index.search(&workspace, "needle-(9|8)[0-9]+", "regex", 0, 50)?;
        Ok(())
    })?;
    let graph = sample(|| {
        index.graph(&workspace, &paths[0], "depth")?;
        Ok(())
    })?;
    let search_index = index.clone();
    let search_workspace = workspace.clone();
    let (search_tx, search_rx) = std::sync::mpsc::channel();
    let search_worker = std::thread::spawn(move || {
        let result = search_index.search(
            &search_workspace,
            "benchmark-absent-pattern-[0-9]+",
            "regex",
            0,
            500,
        );
        let _ = search_tx.send(result);
    });
    std::thread::sleep(Duration::from_millis(2));
    let search_cancel_start = Instant::now();
    index.cancel_search();
    let search_cancel_result = search_rx
        .recv_timeout(Duration::from_secs(6))
        .map_err(|_| AppError::new("BENCH_CANCEL", "Search cancellation exceeded six seconds"))?;
    let search_cancel_ms = millis(search_cancel_start);
    let search_cancelled = search_cancel_result
        .as_ref()
        .err()
        .is_some_and(|error| error.code == "SEARCH_CANCELLED");
    search_worker
        .join()
        .map_err(|_| AppError::new("BENCH_CANCEL", "Search worker panicked"))?;
    let token = Arc::new(AtomicBool::new(false));
    let worker_token = token.clone();
    let worker_index = index.clone();
    let worker_workspace = workspace.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = worker_index.index(&worker_workspace, &worker_token);
        let _ = done_tx.send(result);
    });
    std::thread::sleep(Duration::from_millis(20));
    let start = Instant::now();
    token.store(true, Ordering::Release);
    let cancelled = done_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| AppError::new("BENCH_CANCEL", "Index cancellation exceeded ten seconds"))??;
    let cancellation_ms = millis(start);
    worker
        .join()
        .map_err(|_| AppError::new("BENCH_CANCEL", "Index worker panicked"))?;
    let start = Instant::now();
    let original = workspace.read(&paths[0])?;
    workspace.write(
        &paths[0],
        &format!("{}\nexport const changed=true;", original.content),
        &original.hash,
    )?;
    let incremental = index.update(&workspace, &[paths[0].clone()], &AtomicBool::new(false))?;
    let incremental_ms = millis(start);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let callback_count = callbacks.clone();
    let completed_callbacks = Arc::new(AtomicUsize::new(0));
    let callback_completions = completed_callbacks.clone();
    let watching = index.clone();
    let watched = workspace.clone();
    let watch = watcher::watch(
        &root,
        Arc::new(move |paths, full, cancel| {
            let batch = callback_count.fetch_add(1, Ordering::Relaxed) + 1;
            let callback_start = Instant::now();
            eprintln!(
                "Watcher batch {batch} starting: paths={}, full={full}",
                paths.len()
            );
            let result = if full {
                watching.index(&watched, cancel)
            } else {
                watching.update(&watched, &paths, cancel)
            };
            match result {
                Ok(stats) => eprintln!(
                    "Watcher batch {batch} finished: {:.3} ms, parsed={}, cancelled={}",
                    millis(callback_start),
                    stats.files,
                    stats.cancelled
                ),
                Err(error) => eprintln!("Watcher batch {batch} failed: {}", error.message),
            }
            callback_completions.fetch_add(1, Ordering::Release);
        }),
    )?;
    let start = Instant::now();
    let mut expected = Vec::new();
    for path in paths.iter().take(100) {
        let old = workspace.read(path)?;
        let new = workspace.write(path, &format!("{}\n", old.content), &old.hash)?;
        expected.push((path.clone(), new.hash));
    }
    let writes_ms = millis(start);
    eprintln!("Watcher producer finished 100 writes in {writes_ms:.3} ms");
    let settled = loop {
        let synchronized = db.with(|connection| {
            for (path, hash) in &expected {
                let stored: std::result::Result<String, _> = connection.query_row(
                    "SELECT hash FROM index_files WHERE repository_id=?1 AND path=?2",
                    rusqlite::params![workspace.id, path],
                    |row| row.get(0),
                );
                if stored.as_ref().ok() != Some(hash) {
                    return Ok(false);
                }
            }
            Ok(true)
        })?;
        if synchronized
            && completed_callbacks.load(Ordering::Acquire) == callbacks.load(Ordering::Acquire)
        {
            break true;
        }
        if start.elapsed() > Duration::from_secs(30) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let watcher_ms = millis(start);
    if !settled {
        let missing = db.with(|connection| {
            let mut missing = Vec::new();
            for (path, hash) in &expected {
                let stored: std::result::Result<String, _> = connection.query_row(
                    "SELECT hash FROM index_files WHERE repository_id=?1 AND path=?2",
                    rusqlite::params![workspace.id, path],
                    |row| row.get(0),
                );
                if stored.as_ref().ok() != Some(hash) {
                    missing.push(path.clone());
                }
            }
            Ok(missing)
        })?;
        eprintln!(
            "Watcher deadline: callbacks={}, missing={}, first={:?}",
            callbacks.load(Ordering::Relaxed),
            missing.len(),
            missing.iter().take(3).collect::<Vec<_>>()
        );
    }
    drop(watch);
    if !settled {
        return Err(AppError::new(
            "BENCH_WATCHER",
            "Watcher storm failed to converge within thirty seconds",
        ));
    }
    let vectors = VectorStore::new(db.clone())?;
    let start = Instant::now();
    let mut offset = 0;
    let mut embedded = 0;
    loop {
        let documents = index.documents(&workspace, offset, 128)?;
        if documents.is_empty() {
            break;
        }
        embedded += vectors.update(
            &workspace.id,
            &documents,
            &LocalEmbeddingFixture,
            &AtomicBool::new(false),
        )?;
        offset += documents.len();
    }
    let embedding_ms = millis(start);
    let vector_search = sample(|| {
        vectors.search(
            &workspace.id,
            &LocalEmbeddingFixture.identity(),
            &[1.0; 16],
            None,
            100,
        )?;
        Ok(())
    })?;
    let manager = ProcessManager::new(db.clone());
    let start = Instant::now();
    let id = manager.start(
        &workspace,
        CommandSpec {
            program: std::env::current_exe()?.to_string_lossy().into_owned(),
            args: vec!["--stream-fixture".into()],
            cwd: None,
            env: BTreeMap::new(),
            approved: true,
            is_test: true,
        },
    )?;
    let mut cursor = 0;
    let mut polled = 0;
    let mut truncated = false;
    loop {
        let snapshot = manager.poll(&id, cursor)?;
        polled += snapshot
            .chunks
            .iter()
            .map(|chunk| chunk.text.len())
            .sum::<usize>();
        truncated |= snapshot.truncated;
        cursor = snapshot.next_cursor;
        if !snapshot.running && snapshot.chunks.is_empty() {
            if snapshot.exit_code != Some(0) {
                return Err(AppError::new("BENCH_PROCESS", "Stream fixture failed"));
            }
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            manager.stop(&id)?;
            return Err(AppError::new("BENCH_PROCESS", "Stream fixture timed out"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let process_ms = millis(start);
    let peak = memory(true)?;
    let resident_after = memory(false)?;
    let database_final = database_size(&database_path);
    let result = json!({"sourceFiles":files,"languages":["TypeScript","Python","Rust","JSON","Markdown","JavaScript"],"malformedSourceFiles":(0..files).filter(|number|number%97==0&&matches!(number%6,0|2|5)).count(),"ignoredSourceFiles":1,"oversizedFiles":1,"largeTextBytes":983040,"startupMs":startup_ms,"fixtureGenerationMs":fixture_ms,"indexMs":index_ms,"indexedFiles":indexed.files,"symbols":indexed.symbols,"dependencies":indexed.dependencies,"residentBeforeBytes":resident_before,"residentAfterBytes":resident_after,"peakResidentBytes":peak,"textSearch":text,"symbolSearch":symbol,"regexSearch":regex,"transitiveGraph":graph,"cancelIndexMs":cancellation_ms,"cancelSearchMs":search_cancel_ms,"searchCancellationObserved":search_cancelled,"cancelledIndexReported":cancelled.cancelled,"incrementalUpdateMs":incremental_ms,"incrementalParsedFiles":incremental.files,"watcherStormMs":watcher_ms,"watcherProducerMs":writes_ms,"watcherCallbacks":callbacks.load(Ordering::Relaxed),"watcherChangedFiles":100,"embeddingFixtureMs":embedding_ms,"embeddedDocuments":embedded,"embeddingDimensions":16,"vectorSearch":vector_search,"databaseAfterIndex":database_after_index,"databaseFinal":database_final,"processMs":process_ms,"processGeneratedBytes":16*1024*1024,"processPolledBytes":polled,"processTruncated":truncated});
    drop(manager);
    drop(vectors);
    drop(index);
    drop(workspace);
    drop(db);
    temporary.close()?;
    Ok(result)
}
fn main() -> Result<()> {
    let argument = std::env::args().nth(1).unwrap_or_else(|| "1000".into());
    if argument == "--stream-fixture" {
        let chunk = vec![b'x'; 8192];
        for _ in 0..1024 {
            std::io::stdout().write_all(&chunk)?;
            std::io::stderr().write_all(&chunk)?;
        }
        return Ok(());
    }
    let files = argument
        .parse::<usize>()
        .map_err(|error| AppError::new("BENCH_SIZE", error.to_string()))?;
    if !(1000..=50000).contains(&files) {
        return Err(AppError::new("BENCH_SIZE", "Use 1000 through 50000 files"));
    }
    let result = benchmark(files)?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"schemaVersion":2,"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"profile":if cfg!(debug_assertions){"debug"}else{"release"},"method":"Six language fixtures, malformed ASTs, Unicode/space paths, deep directories, ignored and oversized files; 20 warm query samples; OS process peak RSS; local deterministic 16-dimensional embedding fixture, no network/provider quality measurement; SQLite FULL WAL including WAL and shared-memory sizes","result":result})
        )?
    );
    Ok(())
}

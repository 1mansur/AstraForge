use astraforge_core::database::Database;
use astraforge_core::error::Result;
use astraforge_core::git::GitService;
use astraforge_core::index::IndexService;
use astraforge_core::process::{CommandSpec, ProcessManager};
use astraforge_core::provider::EmbeddingProvider;
use astraforge_core::vector::VectorStore;
use astraforge_core::workspace::Workspace;
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
struct Fixture {
    temporary: tempfile::TempDir,
    db: Arc<Database>,
    workspace: Workspace,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("repository");
        std::fs::create_dir(&root).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .arg(&root)
            .status()
            .unwrap()
            .success());
        let db = Arc::new(Database::open(&temporary.path().join("state.sqlite")).unwrap());
        let workspace = Workspace::open(root.to_str().unwrap(), &db).unwrap();
        Self {
            temporary,
            db,
            workspace,
        }
    }
    fn write(&self, path: &str, text: &str) {
        let absolute = self.workspace.root.join(path);
        std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        std::fs::write(absolute, text).unwrap();
    }
    fn git(&self, args: &[&str]) {
        let result = Command::new("git")
            .current_dir(&self.workspace.root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
#[test]
fn dependency_targets_follow_creation_deletion_and_recreation() {
    let fixture = Fixture::new();
    fixture.write(
        "src/importer.ts",
        "import { value } from './target'; export const result = value;",
    );
    let index = IndexService::new(fixture.db.clone()).unwrap();
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        index
            .graph(&fixture.workspace, "src/importer.ts", "dependencies")
            .unwrap()[0]
            .target,
        "module:./target"
    );
    fixture.write("src/target.ts", "export const value = 1;");
    index
        .update(
            &fixture.workspace,
            &["src/target.ts".into()],
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        index
            .graph(&fixture.workspace, "src/importer.ts", "dependencies")
            .unwrap()[0]
            .target,
        "src/target.ts"
    );
    std::fs::remove_file(fixture.workspace.root.join("src/target.ts")).unwrap();
    index
        .update(
            &fixture.workspace,
            &["src/target.ts".into()],
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        index
            .graph(&fixture.workspace, "src/importer.ts", "dependencies")
            .unwrap()[0]
            .target,
        "module:./target"
    );
    fixture.write("src/target.ts", "export const value = 2;");
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        index
            .graph(&fixture.workspace, "src/importer.ts", "dependencies")
            .unwrap()[0]
            .target,
        "src/target.ts"
    );
}
struct CancellingProvider;
impl EmbeddingProvider for CancellingProvider {
    fn identity(&self) -> String {
        "cancelling-fixture".into()
    }
    fn embed(&self, inputs: &[String], cancel: &AtomicBool) -> Result<Vec<Vec<f32>>> {
        cancel.store(true, Ordering::Release);
        Ok(inputs.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}
#[test]
fn embedding_cancellation_after_provider_return_does_not_commit() {
    let fixture = Fixture::new();
    fixture.write("a.ts", "export const a=1;");
    let index = IndexService::new(fixture.db.clone()).unwrap();
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    let store = VectorStore::new(fixture.db.clone()).unwrap();
    let result = store.update(
        &fixture.workspace.id,
        &index.documents(&fixture.workspace, 0, 10).unwrap(),
        &CancellingProvider,
        &AtomicBool::new(false),
    );
    assert!(result.is_err(), "cancelled provider response was committed");
    let count = fixture
        .db
        .with(|db| {
            Ok(db.query_row("SELECT count(*) FROM embeddings", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn git_filters_cannot_run_during_automatic_diff() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "subprocess_security_probe",
            "--ignored",
            "--nocapture",
        ])
        .env("ASTRAFORGE_HARDENING_MODE", "filter")
        .env("ASTRAFORGE_PROBE_SECRET", "harmless-fixture-credential")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[cfg(windows)]
#[test]
fn safe_git_cannot_resolve_to_batch_script() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "subprocess_security_probe",
            "--ignored",
            "--nocapture",
        ])
        .env("ASTRAFORGE_HARDENING_MODE", "substitution")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
#[ignore = "Isolated environment fixture invoked by security regression tests"]
fn subprocess_security_probe() {
    let mode = std::env::var("ASTRAFORGE_HARDENING_MODE").unwrap();
    let fixture = Fixture::new();
    if mode == "filter" {
        fixture.git(&[
            "config",
            "filter.probe.clean",
            "printf '%s' \"$ASTRAFORGE_PROBE_SECRET\" > filter.marker; printf filtered",
        ]);
        fixture.git(&["config", "user.name", "Fixture"]);
        fixture.git(&["config", "user.email", "fixture@example.invalid"]);
        fixture.write(".gitattributes", "*.txt filter=probe");
        fixture.write("watched.txt", "initial");
        fixture.git(&["add", "--", "watched.txt", ".gitattributes"]);
        fixture.git(&[
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-q",
            "-m",
            "fixture",
        ]);
        let marker = fixture.workspace.root.join("filter.marker");
        std::fs::remove_file(&marker).unwrap();
        fixture.write("watched.txt", "changed and longer");
        let result = GitService::diff(&fixture.workspace, Some("watched.txt"), false);
        assert!(result.is_ok(), "{}", result.unwrap_err().message);
        assert!(
            !marker.exists(),
            "automatic Git diff executed a configured clean filter"
        );
        let manager = ProcessManager::new(fixture.db.clone());
        let id = manager
            .start(
                &fixture.workspace,
                CommandSpec {
                    program: "git".into(),
                    args: vec!["diff".into()],
                    cwd: None,
                    env: BTreeMap::new(),
                    approved: false,
                    is_test: false,
                },
            )
            .unwrap();
        for _ in 0..200 {
            if !manager.poll(&id, 0).unwrap().running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !marker.exists(),
            "safe command Git diff executed a configured clean filter"
        );
    } else if mode == "substitution" {
        let bin = fixture.temporary.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(
            bin.join("git.cmd"),
            "@echo substituted>substitution.marker\r\n",
        )
        .unwrap();
        let original = std::env::var_os("PATH").unwrap();
        let paths = std::iter::once(bin).chain(std::env::split_paths(&original));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        let manager = ProcessManager::new(fixture.db.clone());
        let result = manager.start(
            &fixture.workspace,
            CommandSpec {
                program: "git".into(),
                args: vec!["status".into()],
                cwd: None,
                env: BTreeMap::new(),
                approved: false,
                is_test: false,
            },
        );
        assert!(
            result.is_err(),
            "SAFE Git launched a substituted batch script"
        );
        assert!(!fixture.workspace.root.join("substitution.marker").exists());
    } else {
        panic!("unknown fixture mode");
    }
}
#[test]
fn unicode_positions_use_utf16_columns() {
    let fixture = Fixture::new();
    let source = "const prefix='😀'; export const needle=1;";
    fixture.write("unicode.ts", source);
    let index = IndexService::new(fixture.db.clone()).unwrap();
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    let expected = source[..source.find("needle").unwrap()]
        .encode_utf16()
        .count()
        + 1;
    assert_eq!(
        index
            .search(&fixture.workspace, "needle", "text", 0, 10)
            .unwrap()[0]
            .column,
        expected
    );
    assert_eq!(
        index
            .search(&fixture.workspace, "needle", "symbol", 0, 10)
            .unwrap()[0]
            .column,
        expected
    );
}
fn indexed_snapshot(db: &Database, repository: &str) -> Vec<String> {
    db.with(|connection| {
        let mut values = Vec::new();
        for (table, columns, order) in [
            (
                "index_files",
                "path,content,hash,language,size,parse_errors",
                "path",
            ),
            (
                "index_symbols",
                "path,name,kind,line,column_no,end_line",
                "path,name,kind,line,column_no,end_line",
            ),
            (
                "index_references",
                "path,name,kind,line,column_no",
                "path,name,kind,line,column_no",
            ),
            ("index_edges", "source,target,kind", "source,target,kind"),
            ("index_imports", "source,target,kind", "source,target,kind"),
            ("index_dependency_candidates", "source,path", "source,path"),
        ] {
            let mut statement = connection.prepare(&format!(
                "SELECT json_array({columns}) FROM {table} WHERE repository_id=?1 ORDER BY {order}"
            ))?;
            for row in statement.query_map([repository], |row| row.get::<_, String>(0))? {
                values.push(format!("{table}:{}", row?));
            }
        }
        Ok(values)
    })
    .unwrap()
}
#[test]
fn deterministic_mutations_match_a_clean_index() {
    let fixture = Fixture::new();
    fixture.write(
        "src/main.ts",
        "import {value} from './target0'; export const shared=value;",
    );
    fixture.write(".gitignore", "ignored/\n");
    let index = IndexService::new(fixture.db.clone()).unwrap();
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    let mut state = 0x93a5_u64;
    for step in 0..100 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let slot = (state >> 32) % 12;
        let path = format!("src/target{slot}.ts");
        let mut changed = vec![path.clone()];
        match step % 5 {
            0 | 1 => fixture.write(
                &path,
                &format!("export const value={step}; export function shared(){{ return '😀'; }}"),
            ),
            2 => {
                if fixture.workspace.root.join(&path).exists() {
                    std::fs::remove_file(fixture.workspace.root.join(&path)).unwrap();
                }
            }
            3 => {
                let renamed = format!("src/renamed {slot}.ts");
                if fixture.workspace.root.join(&path).exists() {
                    std::fs::rename(
                        fixture.workspace.root.join(&path),
                        fixture.workspace.root.join(&renamed),
                    )
                    .unwrap();
                }
                changed.push(renamed);
            }
            _ => {
                fixture.write(
                    "src/main.ts",
                    &format!("import {{value}} from './target{slot}'; export const shared=value;"),
                );
                changed.push("src/main.ts".into());
            }
        }
        index
            .update(&fixture.workspace, &changed, &AtomicBool::new(false))
            .unwrap();
        if step % 10 == 9 {
            let clean_db = Arc::new(Database::open(std::path::Path::new(":memory:")).unwrap());
            let clean = IndexService::new(clean_db.clone()).unwrap();
            clean
                .index(&fixture.workspace, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(
                indexed_snapshot(&fixture.db, &fixture.workspace.id),
                indexed_snapshot(&clean_db, &fixture.workspace.id),
                "mutation step {step}"
            );
        }
    }
}
#[test]
fn queued_search_cancellation_applies_to_every_mode() {
    let fixture = Fixture::new();
    fixture.write("needle.ts", "export const needle=1;");
    let index = Arc::new(IndexService::new(fixture.db.clone()).unwrap());
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    for mode in ["symbol", "filename", "text", "regex"] {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let database = fixture.db.clone();
        let hold = std::thread::spawn(move || {
            database
                .with(|_| {
                    ready_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .unwrap()
        });
        ready_rx.recv().unwrap();
        let searching = index.clone();
        let workspace = fixture.workspace.clone();
        let request_cancel = Arc::new(AtomicBool::new(false));
        let search_cancel = request_cancel.clone();
        let search = std::thread::spawn(move || {
            searching.search_cancellable(&workspace, "needle", mode, 0, 10, search_cancel)
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        request_cancel.store(true, Ordering::Release);
        release_tx.send(()).unwrap();
        hold.join().unwrap();
        assert_eq!(
            search.join().unwrap().unwrap_err().code,
            "SEARCH_CANCELLED",
            "{mode}"
        );
        assert_eq!(
            index
                .search(&fixture.workspace, "needle", mode, 0, 10)
                .unwrap()
                .len(),
            1
        );
    }
}
struct BarrierProvider {
    started: std::sync::mpsc::Sender<()>,
    resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl EmbeddingProvider for BarrierProvider {
    fn identity(&self) -> String {
        "concurrent-fixture".into()
    }
    fn embed(&self, inputs: &[String], _: &AtomicBool) -> Result<Vec<Vec<f32>>> {
        self.started.send(()).unwrap();
        self.resume.lock().unwrap().recv().unwrap();
        Ok(inputs.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}
struct ImmediateProvider;
impl EmbeddingProvider for ImmediateProvider {
    fn identity(&self) -> String {
        "concurrent-fixture".into()
    }
    fn embed(&self, inputs: &[String], _: &AtomicBool) -> Result<Vec<Vec<f32>>> {
        Ok(inputs.iter().map(|_| vec![0.0, 1.0]).collect())
    }
}
#[test]
fn stale_embedding_response_cannot_overwrite_a_newer_document() {
    let fixture = Fixture::new();
    fixture.write("a.ts", "export const before=1;");
    let index = IndexService::new(fixture.db.clone()).unwrap();
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    let store = Arc::new(VectorStore::new(fixture.db.clone()).unwrap());
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let old = store.clone();
    let repo = fixture.workspace.id.clone();
    let documents = index.documents(&fixture.workspace, 0, 10).unwrap();
    let pending = std::thread::spawn(move || {
        old.update(
            &repo,
            &documents,
            &BarrierProvider {
                started: started_tx,
                resume: std::sync::Mutex::new(resume_rx),
            },
            &AtomicBool::new(false),
        )
    });
    started_rx.recv().unwrap();
    fixture.write("a.ts", "export const after=2;");
    index
        .update(
            &fixture.workspace,
            &["a.ts".into()],
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        store
            .update(
                &fixture.workspace.id,
                &index.documents(&fixture.workspace, 0, 10).unwrap(),
                &ImmediateProvider,
                &AtomicBool::new(false)
            )
            .unwrap(),
        1
    );
    resume_tx.send(()).unwrap();
    assert_eq!(pending.join().unwrap().unwrap(), 0);
    let results = store
        .search(
            &fixture.workspace.id,
            "concurrent-fixture",
            &[0.0, 1.0],
            None,
            10,
        )
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content.contains("after"));
    assert_eq!(results[0].score, 1.0);
}
#[test]
fn watcher_delivers_supported_paths_and_cancels_its_callback() {
    let fixture = Fixture::new();
    let (sender, receiver) = std::sync::mpsc::channel();
    let watch = astraforge_core::watcher::watch(
        &fixture.workspace.root,
        Arc::new(move |paths, _, _| {
            sender.send(paths).unwrap();
        }),
    )
    .unwrap();
    for path in [
        ".venv/a.py",
        "__pycache__/b.py",
        "src/.astraforge-helper.ts",
    ] {
        fixture.write(path, "export const a=1;");
    }
    let started = std::time::Instant::now();
    let mut paths = std::collections::BTreeSet::new();
    while started.elapsed() < std::time::Duration::from_secs(5) && paths.len() < 3 {
        if let Ok(batch) = receiver.recv_timeout(std::time::Duration::from_millis(100)) {
            for path in batch {
                if path.ends_with(".py") || path.ends_with(".ts") {
                    paths.insert(path);
                }
            }
        }
    }
    assert!(paths.contains(".venv/a.py"));
    assert!(paths.contains("__pycache__/b.py"));
    assert!(paths.contains("src/.astraforge-helper.ts"));
    drop(watch);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (stopped_tx, stopped_rx) = std::sync::mpsc::channel();
    let watch = astraforge_core::watcher::watch(
        &fixture.workspace.root,
        Arc::new(move |_, _, cancel| {
            started_tx.send(()).unwrap();
            while !cancel.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            stopped_tx.send(()).unwrap();
        }),
    )
    .unwrap();
    fixture.write("stop.ts", "const a=1;");
    started_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    drop(watch);
    stopped_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
}
fn stress_spec(mode: &str) -> CommandSpec {
    CommandSpec {
        program: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: vec![
            "--exact".into(),
            "process_stress_helper".into(),
            "--ignored".into(),
            "--nocapture".into(),
        ],
        cwd: None,
        env: BTreeMap::from([("ASTRAFORGE_PROCESS_FIXTURE".into(), mode.into())]),
        approved: true,
        is_test: true,
    }
}
fn wait_command(manager: &ProcessManager, id: &str) {
    let start = std::time::Instant::now();
    loop {
        if !manager.poll(id, 0).unwrap().running {
            return;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
#[test]
fn process_stdin_backpressure_is_bounded_and_stoppable() {
    let fixture = Fixture::new();
    let manager = ProcessManager::new(fixture.db.clone());
    let id = manager
        .start(&fixture.workspace, stress_spec("sleep"))
        .unwrap();
    let started = std::time::Instant::now();
    let mut full = false;
    for _ in 0..32 {
        if let Err(error) = manager.input(&id, &"x".repeat(16384)) {
            assert_eq!(error.code, "STDIN_BUSY");
            full = true;
            break;
        }
    }
    assert!(full);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    manager.stop(&id).unwrap();
    wait_command(&manager, &id);
}
#[test]
fn process_capacity_and_rapid_exits_release_slots() {
    let fixture = Fixture::new();
    let manager = ProcessManager::new(fixture.db.clone());
    let mut running = Vec::new();
    for _ in 0..16 {
        running.push(
            manager
                .start(&fixture.workspace, stress_spec("sleep"))
                .unwrap(),
        );
    }
    assert_eq!(
        manager
            .start(&fixture.workspace, stress_spec("sleep"))
            .unwrap_err()
            .code,
        "PROCESS_LIMIT"
    );
    for id in &running {
        manager.stop(id).unwrap();
    }
    for id in &running {
        wait_command(&manager, id);
    }
    for _ in 0..32 {
        let id = manager
            .start(&fixture.workspace, stress_spec("instant"))
            .unwrap();
        wait_command(&manager, &id);
        assert_eq!(manager.poll(&id, 0).unwrap().exit_code, Some(0));
    }
}
#[test]
fn simultaneous_stdout_stderr_remain_bounded() {
    let fixture = Fixture::new();
    let manager = ProcessManager::new(fixture.db.clone());
    let id = manager
        .start(&fixture.workspace, stress_spec("streams"))
        .unwrap();
    wait_command(&manager, &id);
    let snapshot = manager.poll(&id, 0).unwrap();
    assert!(snapshot.truncated);
    assert!(
        snapshot
            .chunks
            .iter()
            .map(|chunk| chunk.text.len())
            .sum::<usize>()
            <= 128 * 1024 + 8192
    );
    assert_eq!(snapshot.exit_code, Some(0));
}
#[test]
#[ignore = "Isolated subprocess fixture invoked by lifecycle regression tests"]
fn process_stress_helper() {
    use std::io::Write;
    match std::env::var("ASTRAFORGE_PROCESS_FIXTURE")
        .unwrap()
        .as_str()
    {
        "sleep" => std::thread::sleep(std::time::Duration::from_secs(120)),
        "instant" => (),
        "streams" => {
            let out = vec![b'x'; 8192];
            let err = vec![b'y'; 8192];
            for _ in 0..1024 {
                std::io::stdout().write_all(&out).unwrap();
                std::io::stderr().write_all(&err).unwrap();
            }
        }
        _ => panic!("unknown process fixture"),
    }
}
#[test]
fn full_index_refreshes_legacy_ast_positions_without_content_changes() {
    let fixture = Fixture::new();
    let text = "const prefix='😀'; export const needle=1;";
    fixture.write("old.ts", text);
    let index = IndexService::new(fixture.db.clone()).unwrap();
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    fixture
        .db
        .with(|connection| {
            connection.execute("UPDATE index_files SET analysis_version=0", [])?;
            connection.execute("UPDATE index_symbols SET column_no=999", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        index
            .index(&fixture.workspace, &AtomicBool::new(false))
            .unwrap()
            .files,
        1
    );
    assert_eq!(
        index
            .search(&fixture.workspace, "needle", "symbol", 0, 10)
            .unwrap()[0]
            .column,
        text[..text.find("needle").unwrap()].encode_utf16().count() + 1
    );
}
struct MismatchedDimensions;
impl EmbeddingProvider for MismatchedDimensions {
    fn identity(&self) -> String {
        "dimension-fixture".into()
    }
    fn embed(&self, inputs: &[String], _: &AtomicBool) -> Result<Vec<Vec<f32>>> {
        Ok(inputs
            .iter()
            .enumerate()
            .map(|(at, _)| vec![1.0; at + 1])
            .collect())
    }
}
#[test]
fn inconsistent_embedding_dimensions_roll_back_the_batch() {
    let fixture = Fixture::new();
    fixture.write("a.ts", "export const a=1;");
    fixture.write("b.ts", "export const b=2;");
    let index = IndexService::new(fixture.db.clone()).unwrap();
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    let store = VectorStore::new(fixture.db.clone()).unwrap();
    let result = store.update(
        &fixture.workspace.id,
        &index.documents(&fixture.workspace, 0, 10).unwrap(),
        &MismatchedDimensions,
        &AtomicBool::new(false),
    );
    assert_eq!(result.unwrap_err().code, "vector_dimensions");
    assert_eq!(
        fixture
            .db
            .with(|connection| Ok(connection.query_row(
                "SELECT count(*) FROM embeddings",
                [],
                |row| row.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
}
#[test]
fn external_search_token_remembers_cancellation_before_dispatch() {
    let fixture = Fixture::new();
    let index = IndexService::new(fixture.db.clone()).unwrap();
    let cancel = Arc::new(AtomicBool::new(true));
    for mode in ["filename", "symbol", "text", "regex"] {
        assert_eq!(
            index
                .search_cancellable(&fixture.workspace, "missing", mode, 0, 10, cancel.clone())
                .unwrap_err()
                .code,
            "SEARCH_CANCELLED"
        );
    }
    assert!(index
        .search(&fixture.workspace, "missing", "text", 0, 10)
        .unwrap()
        .is_empty());
}
#[test]
fn git_worktree_stays_bound_after_configuration_changes() {
    let fixture = Fixture::new();
    fixture.write("inside.ts", "export const inside=true;");
    fixture.write("nested/child.ts", "export const child=true;");
    let outside = fixture.temporary.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("outside-only.ts"), "outside scope fixture").unwrap();
    fixture.git(&["config", "core.worktree", outside.to_str().unwrap()]);
    let status = GitService::status(&fixture.workspace).unwrap();
    let entries = status["entries"].as_array().unwrap();
    assert!(entries.iter().any(|entry| entry["path"] == "inside.ts"));
    assert!(!entries
        .iter()
        .any(|entry| entry["path"] == "outside-only.ts"));
    let manager = ProcessManager::new(fixture.db.clone());
    let id = manager
        .start(
            &fixture.workspace,
            CommandSpec {
                program: "git".into(),
                args: vec!["status".into(), "--short".into()],
                cwd: Some("nested".into()),
                env: BTreeMap::new(),
                approved: false,
                is_test: false,
            },
        )
        .unwrap();
    wait_command(&manager, &id);
    let output = manager
        .poll(&id, 0)
        .unwrap()
        .chunks
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect::<String>();
    assert!(output.contains("../inside.ts"), "{output}");
    assert!(!output.contains("outside-only"));
}
#[cfg(windows)]
#[test]
fn windows_dependency_targets_use_ordinal_case_and_actual_paths() {
    let fixture = Fixture::new();
    fixture.write("src/importer.ts","import {value} from './МИР'; import {other} from './TARGET'; export const result=value+other;");
    let index = IndexService::new(fixture.db.clone()).unwrap();
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    fixture.write("src/мир.ts", "export const value=1;");
    fixture.write("src/target.ts", "export const other=2;");
    index
        .update(
            &fixture.workspace,
            &["src/мир.ts".into(), "src/target.ts".into()],
            &AtomicBool::new(false),
        )
        .unwrap();
    let mut targets = index
        .graph(&fixture.workspace, "src/importer.ts", "dependencies")
        .unwrap()
        .into_iter()
        .map(|edge| edge.target)
        .collect::<Vec<_>>();
    targets.sort();
    assert_eq!(targets, vec!["src/target.ts", "src/мир.ts"]);
    assert_eq!(
        index
            .graph(&fixture.workspace, "src/мир.ts", "dependents")
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_file(fixture.workspace.root.join("src/мир.ts")).unwrap();
    index
        .update(
            &fixture.workspace,
            &["src/мир.ts".into()],
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(index
        .graph(&fixture.workspace, "src/importer.ts", "dependencies")
        .unwrap()
        .iter()
        .any(|edge| edge.target == "module:./МИР"));
    let clean_db = Arc::new(Database::open(std::path::Path::new(":memory:")).unwrap());
    let clean = IndexService::new(clean_db.clone()).unwrap();
    clean
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        indexed_snapshot(&fixture.db, &fixture.workspace.id),
        indexed_snapshot(&clean_db, &fixture.workspace.id)
    );
}
#[test]
fn dense_markdown_locations_and_complexity_are_bounded() {
    let adapter = astraforge_core::index::adapter_for("dense.md").unwrap();
    let source = "# 😀 heading\n".repeat(50_000);
    let started = std::time::Instant::now();
    let parsed = adapter.parse(&source).unwrap();
    eprintln!(
        "dense Markdown 50000 headings: {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(parsed.symbols.len(), 50_000);
    assert_eq!(parsed.symbols.first().unwrap().line, 1);
    assert_eq!(parsed.symbols.last().unwrap().line, 50_000);
    let oversized = format!("{source}# overflow\n");
    let started = std::time::Instant::now();
    assert_eq!(
        adapter.parse(&oversized).unwrap_err().code,
        "PARSER_COMPLEXITY"
    );
    eprintln!(
        "dense Markdown 50001 heading cap: {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
}
#[test]
fn ignored_atomic_write_events_do_not_force_full_rescans() {
    let fixture = Fixture::new();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let (events_tx, events_rx) = std::sync::mpsc::channel();
    let receiver = std::sync::Mutex::new(resume_rx);
    let batches = std::sync::atomic::AtomicUsize::new(0);
    let watch = astraforge_core::watcher::watch(
        &fixture.workspace.root,
        Arc::new(move |paths, full, _| {
            if batches.fetch_add(1, Ordering::Relaxed) == 0 {
                entered_tx.send(()).unwrap();
                receiver.lock().unwrap().recv().unwrap();
            }
            events_tx.send((paths, full)).unwrap();
        }),
    )
    .unwrap();
    fixture.write("first.ts", "const first=1;");
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    for at in 0..1400 {
        let path = fixture
            .workspace
            .root
            .join(format!(".astraforge-write-{at}.tmp"));
        std::fs::write(&path, "temporary").unwrap();
        std::fs::remove_file(path).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    fixture.write("last.ts", "const last=1;");
    resume_tx.send(()).unwrap();
    let mut flags = Vec::new();
    let start = std::time::Instant::now();
    let mut saw_last = false;
    while start.elapsed() < std::time::Duration::from_secs(3) {
        if let Ok((paths, full)) = events_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            flags.push(full);
            saw_last |= paths.iter().any(|path| path == "last.ts");
            if saw_last {
                break;
            }
        }
    }
    drop(watch);
    assert!(saw_last, "The final real file event was lost: {flags:?}");
    assert!(
        flags.iter().all(|full| !*full),
        "Ignored write traffic forced a full scan: {flags:?}"
    );
}
#[test]
fn atomic_file_updates_do_not_rescan_unchanged_parent_directories() {
    let fixture = Fixture::new();
    for number in 0..100 {
        fixture.write(&format!("nested/deep/{number}.ts"), "const value=1;");
    }
    let (events_tx, events_rx) = std::sync::mpsc::channel();
    let watch = astraforge_core::watcher::watch(
        &fixture.workspace.root,
        Arc::new(move |paths, full, _| {
            events_tx.send((paths, full)).unwrap();
        }),
    )
    .unwrap();
    for number in 0..100 {
        let path = format!("nested/deep/{number}.ts");
        let old = fixture.workspace.read(&path).unwrap();
        fixture
            .workspace
            .write(&path, "const value=2;", &old.hash)
            .unwrap();
    }
    let mut all_paths = std::collections::BTreeSet::new();
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(1) {
        if let Ok((paths, full)) = events_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            assert!(!full);
            all_paths.extend(paths);
        }
    }
    drop(watch);
    eprintln!("Atomic write notification paths: {all_paths:?}");
    assert!(!all_paths.is_empty());
    assert!(
        all_paths.iter().all(|path| path.ends_with(".ts")),
        "Atomic writes scheduled directory rescans: {all_paths:?}"
    );
}
#[test]
fn incremental_file_scopes_do_not_scan_every_repository_row() {
    let fixture = Fixture::new();
    let index = IndexService::new(fixture.db.clone()).unwrap();
    fixture.db.with(|connection| {
        let transaction=connection.transaction()?;
        { let mut files=transaction.prepare("INSERT INTO index_files(repository_id,path,content,hash,language,size,parse_errors,generation,analysis_version) VALUES(?1,?2,'','unchanged','Text',0,0,'seed',2)")?;
        let mut metadata=transaction.prepare("INSERT INTO index_dependency_files(repository_id,path) VALUES(?1,?2)")?;
        let mut candidates=transaction.prepare("INSERT INTO index_dependency_candidates(repository_id,source,path) VALUES(?1,?2,?3)")?;
        for number in 0..50_000 { let path=format!("untouched/{number:06}.txt"); files.execute(rusqlite::params![fixture.workspace.id,path])?;metadata.execute(rusqlite::params![fixture.workspace.id,path])?;candidates.execute(rusqlite::params![fixture.workspace.id,path,format!("unrelated-target/{number:06}.ts")])?; } }
        transaction.commit()?;
        Ok(())
    }).unwrap();
    let paths = (0..100)
        .map(|number| format!("changed/{number:03}.ts"))
        .collect::<Vec<_>>();
    for path in &paths {
        fixture.write(path, "export const changed=true;");
    }
    let steps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let measured = steps.clone();
    fixture
        .db
        .with(|connection| {
            connection.progress_handler(
                1000,
                Some(move || measured.fetch_add(1, Ordering::Relaxed) >= 5000),
            );
            Ok(())
        })
        .unwrap();
    let result = index.update(&fixture.workspace, &paths, &AtomicBool::new(false));
    fixture
        .db
        .with(|connection| {
            connection.progress_handler(0, None::<fn() -> bool>);
            Ok(())
        })
        .unwrap();
    eprintln!("Incremental 100 scopes among 50000 persisted paths/candidates used approximately {} SQLite VM steps",steps.load(Ordering::Relaxed)*1000);
    assert!(
        result.is_ok(),
        "Incremental update exhausted its SQLite work budget: {result:?}"
    );
    assert_eq!(result.unwrap().files, 100);
    assert!(steps.load(Ordering::Relaxed) < 5000);
}
#[test]
fn incremental_directory_ranges_preserve_adjacent_and_unicode_paths() {
    let fixture = Fixture::new();
    for path in [
        "dir/child.ts",
        "dir0/keep.ts",
        "dirSibling/keep.ts",
        "Юникод/child.ts",
        "Юникод0/keep.ts",
    ] {
        fixture.write(path, "export const value=1;");
    }
    let index = IndexService::new(fixture.db.clone()).unwrap();
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .unwrap();
    for directory in ["dir", "Юникод"] {
        std::fs::remove_file(fixture.workspace.root.join(directory).join("child.ts")).unwrap();
        std::fs::remove_dir(fixture.workspace.root.join(directory)).unwrap();
    }
    index
        .update(
            &fixture.workspace,
            &["dir".into(), "Юникод".into()],
            &AtomicBool::new(false),
        )
        .unwrap();
    let remaining = index
        .documents(&fixture.workspace, 0, 20)
        .unwrap()
        .into_iter()
        .map(|(path, _, _)| path)
        .collect::<Vec<_>>();
    assert_eq!(
        remaining,
        vec!["dir0/keep.ts", "dirSibling/keep.ts", "Юникод0/keep.ts"]
    );
    fixture.write("dir/new.ts", "export const recreated=1;");
    fixture.write("Юникод/new.ts", "export const recreated=1;");
    index
        .update(
            &fixture.workspace,
            &[String::new()],
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(index.documents(&fixture.workspace, 0, 20).unwrap().len(), 5);
}
#[test]
fn candidate_covering_index_migrates_without_rebuilding_on_reopen() {
    let fixture = Fixture::new();
    let initial = IndexService::new(fixture.db.clone()).unwrap();
    fixture.db.with(|connection| {
        connection.execute_batch("DROP INDEX index_candidate_ordinal_source;CREATE INDEX index_candidate_ordinal_path ON index_dependency_candidates(repository_id,path COLLATE astra_path)")?;
        connection.execute("INSERT INTO index_dependency_candidates(repository_id,source,path) VALUES(?1,'src/importer.ts','src/target.ts')",[&fixture.workspace.id])?;
        Ok(())
    }).unwrap();
    drop(initial);
    let migrated = IndexService::new(fixture.db.clone()).unwrap();
    let schema_version=fixture.db.with(|connection| {
        let source:String=connection.query_row("SELECT source FROM index_dependency_candidates WHERE repository_id=?1 AND path='src/target.ts' COLLATE astra_path",[&fixture.workspace.id],|row|row.get(0))?;
        assert_eq!(source,"src/importer.ts");
        let mut query=connection.prepare("EXPLAIN QUERY PLAN SELECT source FROM index_dependency_candidates WHERE repository_id=?1 AND path=?2 COLLATE astra_path")?;
        let plans=query.query_map(rusqlite::params![fixture.workspace.id,"src/target.ts"],|row|row.get::<_,String>(3))?.collect::<std::result::Result<Vec<_>,_>>()?;
        assert!(plans.iter().any(|plan|plan.contains("index_candidate_ordinal_source")&&plan.contains("path=?")),"Unexpected candidate plan: {plans:?}");
        Ok(connection.query_row("PRAGMA schema_version",[],|row|row.get::<_,i64>(0))?)
    }).unwrap();
    drop(migrated);
    let _reopened = IndexService::new(fixture.db.clone()).unwrap();
    let reopened_version = fixture
        .db
        .with(|connection| {
            Ok(connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))?)
        })
        .unwrap();
    assert_eq!(schema_version, reopened_version);
}

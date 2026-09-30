use astraforge_core::database::Database;
use astraforge_core::index::IndexService;
use astraforge_core::workspace::Workspace;
use rusqlite::functions::FunctionFlags;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
fn snapshot(database: &Database, repository: &str) -> Vec<String> {
    database.with(|connection| {
        let mut result = Vec::new();
        for (table, columns, order) in [
            ("index_files", "path,content,hash,language,size,parse_errors,analysis_version", "path"),
            ("index_symbols", "path,name,kind,line,column_no,end_line", "path,name,kind,line,column_no,end_line"),
            ("index_references", "path,name,kind,line,column_no", "path,name,kind,line,column_no"),
            ("index_edges", "source,target,kind", "source,target,kind"),
            ("index_imports", "source,target,kind", "source,target,kind"),
            ("index_dependency_candidates", "source,path", "source,path"),
            ("index_dependency_files", "path", "path"),
        ] {
            let mut statement = connection.prepare(&format!("SELECT json_array({columns}) FROM {table} WHERE repository_id=?1 ORDER BY {order}"))?;
            for row in statement.query_map([repository], |row| row.get::<_, String>(0))? {
                result.push(format!("{table}:{}", row?));
            }
        }
        Ok(result)
    }).expect("consistent index snapshot")
}
fn integrity(database: &Database, repository: &str) {
    database.with(|connection| {
        let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        assert_eq!(result, "ok");
        let violations: i64 = connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
        assert_eq!(violations, 0);
        connection.execute("INSERT INTO index_text(index_text,rank) VALUES('integrity-check',1)", [])?;
        for (table, path) in [("index_symbols", "path"), ("index_references", "path"), ("index_edges", "source"), ("index_imports", "source"), ("index_dependency_candidates", "source"), ("index_dependency_files", "path")] {
            let orphans: i64 = connection.query_row(&format!("SELECT COUNT(*) FROM {table} AS child WHERE child.repository_id=?1 AND NOT EXISTS(SELECT 1 FROM index_files AS parent WHERE parent.repository_id=child.repository_id AND parent.path=child.{path})"), [repository], |row| row.get(0))?;
            assert_eq!(orphans, 0, "orphaned rows in {table}");
        }
        Ok(())
    }).expect("SQLite and FTS consistency");
}
#[test]
#[ignore = "Invoked by active_index_transaction_crash_preserves_snapshot_and_reindexes"]
fn index_transaction_crash_helper() {
    let root =
        PathBuf::from(std::env::var_os("ASTRAFORGE_INDEX_CRASH_ROOT").expect("isolated root"));
    let path = PathBuf::from(
        std::env::var_os("ASTRAFORGE_INDEX_CRASH_DATABASE").expect("isolated database"),
    );
    let marker =
        PathBuf::from(std::env::var_os("ASTRAFORGE_INDEX_CRASH_MARKER").expect("crash marker"));
    let database = Arc::new(Database::open(&path).expect("child database"));
    let workspace =
        Workspace::open(root.to_str().expect("UTF8 root"), &database).expect("child workspace");
    let index = IndexService::new(database.clone()).expect("child index");
    database.with(|connection| {
        connection.create_scalar_function("terminate_index_transaction", 0, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS, move |_| -> rusqlite::Result<i64> {
            std::fs::write(&marker, "after symbol insert before transaction commit").expect("record exact crash boundary");
            std::fs::OpenOptions::new().write(true).open(&marker).expect("marker handle").sync_all().expect("flush crash evidence");
            std::process::exit(74);
        })?;
        connection.execute_batch("CREATE TEMP TRIGGER crash_active_index AFTER INSERT ON index_symbols WHEN NEW.path='a.ts' AND NEW.name='afterCrash' BEGIN SELECT terminate_index_transaction(); END;")?;
        Ok(())
    }).expect("install transaction crash trigger");
    let result = index.index(&workspace, &AtomicBool::new(false));
    panic!("index transaction crash trigger was not reached: {result:?}");
}
#[test]
fn active_index_transaction_crash_preserves_snapshot_and_reindexes() {
    let temporary = tempfile::tempdir().expect("isolated index crash fixture");
    let root = temporary.path().join("repository");
    let path = temporary.path().join("state.db");
    let marker = temporary.path().join("crash-boundary.txt");
    std::fs::create_dir(&root).expect("repository directory");
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .arg(&root)
        .output()
        .expect("Git fixture");
    assert!(
        git.status.success(),
        "{}",
        String::from_utf8_lossy(&git.stderr)
    );
    for (name, content) in [
        ("a.ts", "import { beforeTarget } from './target'; export function beforeEntry(){return beforeTarget();}"),
        ("target.ts", "export function beforeTarget(){return 'beforeSentinel';}"),
        ("removed.ts", "export const removedSentinel=1;"),
        ("README.md", "# Stable fixture\n"),
    ] {
        std::fs::write(root.join(name), content).expect("initial indexed content");
    }
    let (prior, completed_id) = {
        let database = Arc::new(Database::open(&path).expect("initial database"));
        let workspace = Workspace::open(root.to_str().expect("UTF8 root"), &database)
            .expect("initial workspace");
        let index = IndexService::new(database.clone()).expect("initial index");
        assert_eq!(
            index
                .index(&workspace, &AtomicBool::new(false))
                .expect("completed initial index")
                .files,
            4
        );
        integrity(&database, &workspace.id);
        let completed_id = database
            .with(|connection| {
                Ok(connection.query_row(
                    "SELECT id FROM index_runs WHERE repository_id=?1 AND status='completed'",
                    [&workspace.id],
                    |row| row.get::<_, String>(0),
                )?)
            })
            .expect("completed generation");
        (snapshot(&database, &workspace.id), completed_id)
    };
    std::fs::write(root.join("a.ts"), "import { afterTarget } from './renamed'; export function afterCrash(){return afterTarget();}").expect("external source edit");
    std::fs::rename(root.join("target.ts"), root.join("renamed.ts")).expect("external rename");
    std::fs::write(
        root.join("renamed.ts"),
        "export function afterTarget(){return 'afterSentinel';}",
    )
    .expect("external target edit");
    std::fs::remove_file(root.join("removed.ts")).expect("external deletion");
    std::fs::write(root.join("new.ts"), "export const newSentinel=2;").expect("external creation");
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "index_transaction_crash_helper",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("ASTRAFORGE_INDEX_CRASH_ROOT", &root)
        .env("ASTRAFORGE_INDEX_CRASH_DATABASE", &path)
        .env("ASTRAFORGE_INDEX_CRASH_MARKER", &marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("actual crash subprocess");
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().expect("child status").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("terminate stalled fixture");
            let output = child.wait_with_output().expect("reap stalled fixture");
            panic!(
                "index crash fixture timed out: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("reap crashed fixture");
    assert_eq!(
        output.status.code(),
        Some(74),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("durable crash marker"),
        "after symbol insert before transaction commit"
    );
    let database = Arc::new(Database::open(&path).expect("reopen after abrupt exit"));
    let workspace =
        Workspace::open(root.to_str().expect("UTF8 root"), &database).expect("reopened workspace");
    let index = IndexService::new(database.clone()).expect("mark interrupted index run");
    assert_eq!(
        snapshot(&database, &workspace.id),
        prior,
        "uncommitted index changes must not replace the prior complete snapshot"
    );
    integrity(&database, &workspace.id);
    database.with(|connection| {
        let generations: i64 = connection.query_row("SELECT COUNT(*) FROM index_files WHERE repository_id=?1 AND generation<>?2", rusqlite::params![workspace.id, completed_id], |row| row.get(0))?;
        assert_eq!(generations, 0);
        let interrupted: i64 = connection.query_row("SELECT COUNT(*) FROM index_runs WHERE repository_id=?1 AND status='interrupted'", [&workspace.id], |row| row.get(0))?;
        assert_eq!(interrupted, 1);
        let incomplete: i64 = connection.query_row("SELECT COUNT(*) FROM index_runs WHERE repository_id=?1 AND status NOT IN('completed','interrupted')", [&workspace.id], |row| row.get(0))?;
        assert_eq!(incomplete, 0);
        Ok(())
    }).expect("interrupted run and completed generation coexist");
    assert_eq!(
        index
            .search(&workspace, "beforeSentinel", "text", 0, 10)
            .expect("prior FTS entry")
            .len(),
        1
    );
    assert!(index
        .search(&workspace, "afterSentinel", "text", 0, 10)
        .expect("uncommitted text absent")
        .is_empty());
    assert!(
        !index
            .index(&workspace, &AtomicBool::new(false))
            .expect("reconcile actual changed files")
            .cancelled
    );
    integrity(&database, &workspace.id);
    let clean_database =
        Arc::new(Database::open(Path::new(":memory:")).expect("independent clean database"));
    let clean_workspace = Workspace::open(root.to_str().expect("UTF8 root"), &clean_database)
        .expect("independent workspace");
    let clean_index = IndexService::new(clean_database.clone()).expect("independent clean index");
    clean_index
        .index(&clean_workspace, &AtomicBool::new(false))
        .expect("independent full index");
    assert_eq!(
        snapshot(&database, &workspace.id),
        snapshot(&clean_database, &clean_workspace.id),
        "recovery followed by reindex must converge to a clean index"
    );
    database
        .with(|connection| {
            let mut statement = connection.prepare(
                "SELECT path,hash FROM index_files WHERE repository_id=?1 ORDER BY path",
            )?;
            let rows = statement
                .query_map([&workspace.id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            assert_eq!(rows.len(), 4);
            for (path, hash) in rows {
                assert_eq!(
                    workspace.read(&path)?.hash,
                    hash,
                    "persisted hash for {path}"
                );
            }
            Ok(())
        })
        .expect("every recovered hash matches disk");
    assert!(index
        .search(&workspace, "beforeSentinel", "text", 0, 10)
        .expect("old FTS removed")
        .is_empty());
    assert_eq!(
        index
            .search(&workspace, "afterSentinel", "text", 0, 10)
            .expect("new FTS entry")
            .len(),
        1
    );
}

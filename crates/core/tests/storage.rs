use astraforge_core::database::Database;
use astraforge_core::patch::{PatchProposal, PatchService};
use astraforge_core::workspace::{content_hash, normalize_path, Workspace, MAX_TEXT_BYTES};
use proptest::prelude::*;
use rusqlite::params;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
struct Fixture {
    workspace: Workspace,
    database: Arc<Database>,
    patches: PatchService,
    _directory: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary repository");
        let root = directory.path().join("repository");
        std::fs::create_dir(&root).expect("create repository");
        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .arg(&root)
            .status()
            .expect("git available")
            .success());
        let database =
            Arc::new(Database::open(&directory.path().join("state.db")).expect("database"));
        let workspace =
            Workspace::open(root.to_str().expect("UTF-8 path"), &database).expect("workspace");
        let patches = PatchService::new(database.clone());
        Self {
            _directory: directory,
            workspace,
            database,
            patches,
        }
    }
    fn file(&self, path: &str, content: &str) {
        std::fs::write(self.workspace.root.join(path), content).expect("write fixture");
    }
    fn proposal(&self, path: &str, content: Option<&str>) -> PatchProposal {
        PatchProposal {
            path: path.into(),
            content: content.map(str::to_owned),
            expected_hash: self.workspace.read(path).ok().map(|file| file.hash),
        }
    }
    fn interrupted(&self, patch_id: &str, path: &str, before: &str, after: &str) {
        self.database.with(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute("UPDATE patch_sets SET status='applying' WHERE id=?1", [patch_id])?;
            let items = serde_json::json!([{"path":path,"before":before,"after":after}]).to_string();
            transaction.execute("INSERT INTO patch_journal(patch_id,repository_id,prior_status,operation,items,started_at) VALUES(?1,?2,'proposed','applying',?3,0)", params![patch_id, self.workspace.id, items])?;
            transaction.commit()?;
            Ok(())
        }).expect("journal write");
    }
}
#[test]
fn database_migrations_and_state_survive_reopening() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("nested/state.db");
    {
        let database = Database::open(&path).expect("migration");
        database
            .with(|connection| {
                assert_eq!(
                    connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?,
                    1
                );
                connection.execute("INSERT INTO settings(key,value) VALUES('theme','dark')", [])?;
                Ok(())
            })
            .expect("state insert");
    }
    let database = Database::open(&path).expect("reopen");
    database
        .with(|connection| {
            assert_eq!(
                connection.query_row(
                    "SELECT value FROM settings WHERE key='theme'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "dark"
            );
            Ok(())
        })
        .expect("state restored");
}
#[test]
fn database_rejects_a_future_schema() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("state.db");
    let connection = rusqlite::Connection::open(&path).expect("database");
    connection
        .pragma_update(None, "user_version", 999)
        .expect("schema marker");
    drop(connection);
    assert!(matches!(Database::open(&path), Err(error) if error.code == "DB_NEWER_SCHEMA"));
}
#[test]
fn database_exclusive_instance_lock_releases_on_close() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("state.db");
    let first = Database::open(&path).expect("first instance");
    assert!(matches!(Database::open(&path), Err(error) if error.code == "DB_INSTANCE_LOCKED"));
    drop(first);
    assert!(Database::open(&path).is_ok());
}
#[test]
fn workspace_mutations_are_real_and_optimistic() {
    let fixture = Fixture::new();
    fixture
        .workspace
        .create("src", true)
        .expect("directory create");
    fixture
        .workspace
        .create("src/main.rs", false)
        .expect("file create");
    let initial = fixture.workspace.read("src/main.rs").expect("initial read");
    let edited = fixture
        .workspace
        .write("src/main.rs", "fn main() {}\n", &initial.hash)
        .expect("save");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.root.join("src/main.rs")).expect("actual file"),
        edited.content
    );
    assert_eq!(
        fixture
            .workspace
            .write("src/main.rs", "stale", &initial.hash)
            .expect_err("conflict")
            .code,
        "STALE_FILE"
    );
    fixture
        .workspace
        .rename("src/main.rs", "src/lib.rs")
        .expect("rename");
    assert_eq!(
        fixture.workspace.tree("src").expect("tree")[0].path,
        "src/lib.rs"
    );
    assert_eq!(
        fixture
            .workspace
            .remove("src/lib.rs", None)
            .expect_err("hash required")
            .code,
        "HASH_REQUIRED"
    );
    assert!(fixture.workspace.remove("src", None).is_err());
    fixture
        .workspace
        .remove("src/lib.rs", Some(&edited.hash))
        .expect("remove file");
    fixture
        .workspace
        .remove("src", None)
        .expect("remove empty directory");
    assert!(fixture.workspace.tree("").expect("tree").is_empty());
}
#[test]
fn traversal_git_metadata_and_windows_aliases_are_rejected() {
    let fixture = Fixture::new();
    for path in [
        "../outside",
        "src/../../outside",
        "C:\\secret",
        "/etc/passwd",
        "\\\\server\\share",
        ".git/config",
        ".GIT/config",
        "git~1/config",
        "a/./b",
        "file:secret",
        "file. ",
        "NUL",
        "COM1.txt",
        "dir//file",
        "a\0b",
    ] {
        assert!(
            fixture.workspace.resolve(path).is_err(),
            "accepted forbidden path: {path:?}"
        );
    }
    assert!(fixture.workspace.resolve("src/file.rs").is_err());
    assert!(fixture.workspace.resolve("README.md").is_ok());
    assert!(fixture.workspace.remove("", None).is_err());
}
#[test]
fn text_limits_binary_and_encoding_are_enforced() {
    let fixture = Fixture::new();
    fixture.file("binary", "a\0b");
    assert_eq!(
        fixture
            .workspace
            .read("binary")
            .expect_err("binary rejected")
            .code,
        "BINARY_FILE"
    );
    std::fs::write(fixture.workspace.root.join("invalid"), [0xff, 0xfe, 0xab])
        .expect("non-UTF8 file");
    assert_eq!(
        fixture
            .workspace
            .read("invalid")
            .expect_err("encoding rejected")
            .code,
        "UNSUPPORTED_ENCODING"
    );
    fixture.file("large", &"x".repeat(MAX_TEXT_BYTES + 1));
    assert_eq!(
        fixture
            .workspace
            .read("large")
            .expect_err("large file rejected")
            .code,
        "FILE_TOO_LARGE"
    );
    fixture.file("bom", "\u{feff}hello");
    let bom = fixture.workspace.read("bom").expect("BOM file");
    assert_eq!(bom.encoding, "utf-8-bom");
    assert_eq!(bom.hash, content_hash("\u{feff}hello"));
}
#[cfg(unix)]
#[test]
fn symlinks_cannot_escape_or_alias_workspace_paths() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().expect("outside directory");
    std::fs::write(outside.path().join("secret"), "outside").expect("outside file");
    std::os::unix::fs::symlink(outside.path(), fixture.workspace.root.join("escape"))
        .expect("symlink");
    fixture.file("inside", "inside");
    std::os::unix::fs::symlink("inside", fixture.workspace.root.join("alias"))
        .expect("internal symlink");
    assert!(fixture.workspace.read("escape/secret").is_err());
    assert!(fixture.workspace.create("escape/created", false).is_err());
    assert!(fixture.workspace.read("alias").is_err());
    assert!(!outside.path().join("created").exists());
}
#[test]
fn patches_create_modify_delete_and_revert_with_persisted_diffs() {
    let fixture = Fixture::new();
    fixture.file("old.txt", "before\n");
    fixture.file("delete.txt", "remove\n");
    let patch = fixture
        .patches
        .propose(
            &fixture.workspace,
            vec![
                fixture.proposal("old.txt", Some("after\n")),
                fixture.proposal("new.txt", Some("new\n")),
                fixture.proposal("delete.txt", None),
            ],
            "test",
        )
        .expect("propose");
    assert!(patch.changes[0].diff.contains("-before"));
    assert!(patch.changes[0].diff.contains("+after"));
    assert_eq!(
        fixture
            .workspace
            .read("old.txt")
            .expect("unapplied")
            .content,
        "before\n"
    );
    assert_eq!(
        fixture
            .patches
            .apply(&fixture.workspace, &patch.id)
            .expect("apply")
            .status,
        "applied"
    );
    assert_eq!(
        fixture.workspace.read("old.txt").expect("modified").content,
        "after\n"
    );
    assert_eq!(
        fixture.workspace.read("new.txt").expect("created").content,
        "new\n"
    );
    assert!(!fixture.workspace.root.join("delete.txt").exists());
    assert_eq!(
        fixture
            .patches
            .revert(&fixture.workspace, &patch.id)
            .expect("revert")
            .status,
        "reverted"
    );
    assert_eq!(
        fixture.workspace.read("old.txt").expect("restored").content,
        "before\n"
    );
    assert_eq!(
        fixture
            .workspace
            .read("delete.txt")
            .expect("restored deletion")
            .content,
        "remove\n"
    );
    assert!(!fixture.workspace.root.join("new.txt").exists());
    assert_eq!(
        fixture
            .patches
            .list(&fixture.workspace)
            .expect("persisted patches")[0]
            .status,
        "reverted"
    );
}
#[cfg(windows)]
#[test]
fn windows_junctions_are_rejected() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().expect("outside directory");
    let link = fixture.workspace.root.join("escape");
    std::fs::write(outside.path().join("secret"), "outside").expect("outside file");
    let result = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:ASTRAFORGE_TEST_LINK -Target $env:ASTRAFORGE_TEST_TARGET | Out-Null"])
        .env("ASTRAFORGE_TEST_LINK", &link).env("ASTRAFORGE_TEST_TARGET", outside.path()).output().expect("junction fixture command");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let read = fixture.workspace.read("escape/secret");
    let create = fixture.workspace.create("escape/created", false);
    let listing = fixture.workspace.tree("").expect("root tree");
    std::fs::remove_dir(&link).expect("remove junction");
    assert!(read.is_err());
    assert!(create.is_err());
    assert!(listing.iter().all(|entry| entry.name != "escape"));
    assert!(!outside.path().join("created").exists());
}
#[test]
fn stale_multifile_patches_leave_every_file_untouched() {
    let fixture = Fixture::new();
    fixture.file("one", "first");
    fixture.file("two", "second");
    let patch = fixture
        .patches
        .propose(
            &fixture.workspace,
            vec![
                fixture.proposal("one", Some("updated one")),
                fixture.proposal("two", Some("updated two")),
            ],
            "test",
        )
        .expect("propose");
    fixture.file("two", "external edit");
    assert_eq!(
        fixture
            .patches
            .apply(&fixture.workspace, &patch.id)
            .expect_err("stale rejected")
            .code,
        "PATCH_STALE"
    );
    assert_eq!(
        fixture
            .workspace
            .read("one")
            .expect("unchanged first")
            .content,
        "first"
    );
    assert_eq!(
        fixture
            .workspace
            .read("two")
            .expect("preserved external edit")
            .content,
        "external edit"
    );
    assert_eq!(
        fixture.patches.list(&fixture.workspace).expect("state")[0].status,
        "proposed"
    );
}
#[test]
fn failed_later_write_rolls_back_previous_files() {
    let fixture = Fixture::new();
    fixture.file("one", "first");
    fixture.file("two", "second");
    let patch = fixture
        .patches
        .propose(
            &fixture.workspace,
            vec![
                fixture.proposal("one", Some("updated one")),
                fixture.proposal("two", Some("updated two")),
            ],
            "test",
        )
        .expect("propose");
    let path = fixture.workspace.root.join("two");
    let original_permissions = std::fs::metadata(&path).expect("permissions").permissions();
    let mut readonly = original_permissions.clone();
    readonly.set_readonly(true);
    std::fs::set_permissions(&path, readonly).expect("read-only fixture");
    let result = fixture.patches.apply(&fixture.workspace, &patch.id);
    std::fs::set_permissions(&path, original_permissions).expect("restore permissions");
    assert_eq!(
        result.expect_err("rollback error").code,
        "PATCH_ROLLED_BACK"
    );
    assert_eq!(
        fixture.workspace.read("one").expect("rolled back").content,
        "first"
    );
    assert_eq!(
        fixture.workspace.read("two").expect("unchanged").content,
        "second"
    );
    assert_eq!(
        fixture
            .patches
            .list(&fixture.workspace)
            .expect("retryable patch")[0]
            .status,
        "proposed"
    );
}
#[test]
fn interrupted_patch_is_recovered_from_durable_journal() {
    let fixture = Fixture::new();
    fixture.file("one", "before");
    let patch = fixture
        .patches
        .propose(
            &fixture.workspace,
            vec![fixture.proposal("one", Some("after"))],
            "test",
        )
        .expect("propose");
    fixture.interrupted(&patch.id, "one", "before", "after");
    fixture.file("one", "after");
    let restarted = PatchService::new(fixture.database.clone());
    assert_eq!(
        restarted.recover(&fixture.workspace).expect("recover"),
        vec![patch.id]
    );
    assert_eq!(
        fixture.workspace.read("one").expect("restored").content,
        "before"
    );
    assert_eq!(
        restarted.list(&fixture.workspace).expect("retryable")[0].status,
        "proposed"
    );
    assert!(restarted
        .recover(&fixture.workspace)
        .expect("idempotent recovery")
        .is_empty());
}
#[test]
fn recovery_never_overwrites_external_changes() {
    let fixture = Fixture::new();
    fixture.file("one", "before");
    let patch = fixture
        .patches
        .propose(
            &fixture.workspace,
            vec![fixture.proposal("one", Some("after"))],
            "test",
        )
        .expect("propose");
    fixture.interrupted(&patch.id, "one", "before", "after");
    fixture.file("one", "external");
    assert_eq!(
        fixture
            .patches
            .recover(&fixture.workspace)
            .expect_err("conflict")
            .code,
        "PATCH_RECOVERY_REQUIRED"
    );
    assert_eq!(
        fixture
            .workspace
            .read("one")
            .expect("preserved external content")
            .content,
        "external"
    );
    assert_eq!(
        fixture
            .patches
            .list(&fixture.workspace)
            .expect("recovery status")[0]
            .status,
        "recovery_required"
    );
    assert_eq!(
        fixture
            .patches
            .propose(
                &fixture.workspace,
                vec![fixture.proposal("one", Some("next"))],
                "test"
            )
            .expect_err("block while journal exists")
            .code,
        "PATCH_RECOVERY_REQUIRED"
    );
    fixture.file("one", "after");
    fixture
        .patches
        .recover(&fixture.workspace)
        .expect("retry after manual resolution");
    assert_eq!(
        fixture.workspace.read("one").expect("restored").content,
        "before"
    );
}
#[test]
fn reject_and_repository_isolation_enforce_patch_states() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    fixture.file("one", "before");
    let patch = fixture
        .patches
        .propose(
            &fixture.workspace,
            vec![fixture.proposal("one", Some("after"))],
            "test",
        )
        .expect("propose");
    assert_eq!(
        fixture
            .patches
            .apply(&other.workspace, &patch.id)
            .expect_err("cross-repository rejected")
            .code,
        "PATCH_NOT_FOUND"
    );
    assert_eq!(
        fixture
            .patches
            .reject(&fixture.workspace, &patch.id)
            .expect("reject")
            .status,
        "rejected"
    );
    assert_eq!(
        fixture
            .patches
            .apply(&fixture.workspace, &patch.id)
            .expect_err("rejected patch cannot apply")
            .code,
        "PATCH_STATE"
    );
}
#[test]
#[ignore = "performance measurement"]
fn patch_benchmark_100_files() {
    let fixture = Fixture::new();
    let before = "fn before() { println!(\"before\"); }\n".repeat(100);
    let after = before.replace("before", "after");
    for number in 0..100 {
        fixture.file(&format!("file-{number}.rs"), &before);
    }
    let proposals = (0..100)
        .map(|number| fixture.proposal(&format!("file-{number}.rs"), Some(&after)))
        .collect();
    let started = std::time::Instant::now();
    let patch = fixture
        .patches
        .propose(&fixture.workspace, proposals, "benchmark")
        .expect("propose");
    let propose_ms = started.elapsed().as_millis();
    let started = std::time::Instant::now();
    fixture
        .patches
        .apply(&fixture.workspace, &patch.id)
        .expect("apply");
    let apply_ms = started.elapsed().as_millis();
    let started = std::time::Instant::now();
    fixture.patches.list(&fixture.workspace).expect("query");
    let sqlite_query_ms = started.elapsed().as_millis();
    let started = std::time::Instant::now();
    fixture
        .patches
        .revert(&fixture.workspace, &patch.id)
        .expect("revert");
    println!(
        "{}",
        serde_json::json!({"files": 100, "bytesPerFile": before.len(), "proposeMs": propose_ms, "applyMs": apply_ms, "sqliteQueryMs": sqlite_query_ms, "revertMs": started.elapsed().as_millis()})
    );
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn normalized_paths_cannot_traverse(components in prop::collection::vec("[a-zA-Z0-9_-]{1,12}", 1..6)) {
        let input = components.join("/");
        if let Ok(normalized) = normalize_path(&input) {
            prop_assert!(!Path::new(&normalized).is_absolute());
            prop_assert!(Path::new(&normalized).components().all(|component| matches!(component, std::path::Component::Normal(_))));
            prop_assert_eq!(&normalized, &input);
            let prefixed = format!("../{input}");
            let suffixed = format!("{input}/../secret");
            prop_assert!(normalize_path(&prefixed).is_err());
            prop_assert!(normalize_path(&suffixed).is_err());
        }
    }
    #[test]
    fn applying_then_reverting_is_identity(before in "[a-zA-Z0-9\n ]{0,150}", after in "[a-zA-Z0-9\n ]{0,150}") {
        prop_assume!(before != after);
        let fixture = Fixture::new();
        fixture.file("roundtrip", &before);
        let patch = fixture.patches.propose(&fixture.workspace, vec![fixture.proposal("roundtrip", Some(&after))], "property").expect("propose");
        fixture.patches.apply(&fixture.workspace, &patch.id).expect("apply");
        prop_assert_eq!(fixture.workspace.read("roundtrip").expect("applied file").content, after);
        fixture.patches.revert(&fixture.workspace, &patch.id).expect("revert");
        prop_assert_eq!(fixture.workspace.read("roundtrip").expect("reverted file").content, before);
    }
}

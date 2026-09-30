use astraforge_core::database::Database;
use astraforge_core::error::AppError;
use astraforge_core::patch::{PatchProposal, PatchService};
use astraforge_core::workspace::{content_hash, normalize_path, Workspace, MAX_TEXT_BYTES};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use rusqlite::functions::FunctionFlags;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;
struct Fixture {
    root: PathBuf,
    database: Arc<Database>,
    workspace: Workspace,
    patches: Arc<PatchService>,
    _directory: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary repository");
        let root = directory.path().join("repository");
        std::fs::create_dir(&root).expect("create repository");
        initialize_git(&root);
        let path = directory.path().join("state.db");
        let database = Arc::new(Database::open(&path).expect("database"));
        let workspace =
            Workspace::open(root.to_str().expect("UTF8 path"), &database).expect("workspace");
        Self {
            root,
            workspace,
            patches: Arc::new(PatchService::new(database.clone())),
            database,
            _directory: directory,
        }
    }
    fn proposal(&self, path: &str, before: &str, after: &str) -> PatchProposal {
        std::fs::write(self.root.join(path), before).expect("fixture file");
        PatchProposal {
            path: path.into(),
            content: Some(after.into()),
            expected_hash: Some(content_hash(before)),
        }
    }
    fn journal_count(&self) -> i64 {
        self.database
            .with(|connection| {
                Ok(connection
                    .query_row("SELECT COUNT(*) FROM patch_journal", [], |row| row.get(0))?)
            })
            .expect("journal count")
    }
    fn verify_integrity(&self) {
        verify_integrity(&self.database);
    }
}
fn initialize_git(root: &Path) {
    let output = Command::new("git")
        .args(["init", "--quiet"])
        .arg(root)
        .output()
        .expect("git available");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn git_fixture_command(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git fixture command");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn reject_git_worktree_redirect(to_ancestor: bool) {
    let fixture = Fixture::new();
    let destination = if to_ancestor {
        fixture._directory.path().to_path_buf()
    } else {
        let path = fixture._directory.path().join("outside");
        std::fs::create_dir(&path).expect("outside directory");
        path
    };
    std::fs::write(
        destination.join("private.txt"),
        "outside the selected repository",
    )
    .expect("outside file");
    git_fixture_command(
        &fixture.root,
        &[
            "config",
            "core.worktree",
            destination.to_str().expect("UTF8 destination"),
        ],
    );
    let opened = Workspace::open(
        fixture.root.to_str().expect("UTF8 repository"),
        &fixture.database,
    );
    assert!(
        matches!(opened, Err(ref error) if error.code == "PATH_REPOSITORY_REDIRECT"),
        "a local Git worktree override must not grant a different filesystem root"
    );
    fixture
        .database
        .with(|connection| {
            let count: i64 =
                connection.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;
            assert_eq!(count, 1, "a rejected root must not be persisted");
            Ok(())
        })
        .expect("repository count");
    assert_eq!(
        std::fs::read_to_string(destination.join("private.txt")).expect("outside file remains"),
        "outside the selected repository"
    );
}
#[test]
fn workspace_scope_rejects_git_redirect_to_sibling() {
    reject_git_worktree_redirect(false);
}
#[test]
fn workspace_scope_rejects_git_redirect_to_ancestor() {
    reject_git_worktree_redirect(true);
}
#[test]
fn workspace_scope_accepts_repository_subdirectories_and_linked_worktrees() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root.join("nested")).expect("nested directory");
    std::fs::write(fixture.root.join("nested/source.txt"), "original tree")
        .expect("tracked fixture");
    git_fixture_command(&fixture.root, &["add", "nested/source.txt"]);
    git_fixture_command(
        &fixture.root,
        &[
            "-c",
            "user.name=Scope Test",
            "-c",
            "user.email=scope@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "scope fixture",
        ],
    );
    let nested = Workspace::open(
        fixture.root.join("nested").to_str().expect("UTF8 nested"),
        &fixture.database,
    )
    .expect("normal subdirectory");
    assert_eq!(nested.root, fixture.workspace.root);
    assert_eq!(nested.id, fixture.workspace.id);
    let linked = fixture._directory.path().join("linked");
    git_fixture_command(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked-scope-fixture",
            linked.to_str().expect("UTF8 linked"),
        ],
    );
    assert!(linked.join(".git").is_file());
    let workspace = Workspace::open(
        linked.join("nested").to_str().expect("UTF8 linked nested"),
        &fixture.database,
    )
    .expect("linked worktree subdirectory");
    assert_eq!(
        workspace.root,
        std::fs::canonicalize(&linked).expect("canonical linked")
    );
    assert_ne!(workspace.id, fixture.workspace.id);
    assert_eq!(
        workspace
            .read("nested/source.txt")
            .expect("linked content")
            .content,
        "original tree"
    );
    workspace
        .write(
            "nested/source.txt",
            "linked tree",
            &content_hash("original tree"),
        )
        .expect("linked write");
    assert_eq!(
        fixture
            .workspace
            .read("nested/source.txt")
            .expect("original content")
            .content,
        "original tree"
    );
}
#[test]
fn workspace_scope_rejects_bare_layouts() {
    let directory = tempfile::tempdir().expect("temporary bare fixture");
    let root = directory.path().join("bare");
    std::fs::create_dir(&root).expect("bare directory");
    git_fixture_command(&root, &["init", "--bare", "--quiet"]);
    let database =
        Database::open(&directory.path().join("state.db")).expect("bare fixture database");
    assert!(
        matches!(Workspace::open(root.to_str().expect("UTF8 bare"), &database), Err(ref error) if error.code == "NOT_GIT_REPOSITORY")
    );
}
#[cfg(windows)]
#[test]
#[ignore = "Invoked in isolated processes by workspace_discovery_rejects_wrappers_and_repository_path_binaries"]
fn workspace_discovery_helper() {
    let fixture = Fixture::new();
    let mode = std::env::var("ASTRAFORGE_DISCOVERY_MODE").expect("isolated discovery mode");
    let bin = if mode == "wrapper" {
        fixture._directory.path().join("bin")
    } else {
        fixture.root.join("bin")
    };
    std::fs::create_dir(&bin).expect("fixture bin");
    let marker = fixture._directory.path().join("executed.txt");
    if mode == "wrapper" {
        std::fs::write(
            bin.join("git.cmd"),
            format!(
                "@echo off\r\necho executed>\"{}\"\r\necho {}\r\n",
                marker.display(),
                fixture.root.display()
            ),
        )
        .expect("untrusted wrapper");
    } else {
        std::fs::copy(
            std::env::current_exe().expect("test binary"),
            bin.join("git.exe"),
        )
        .expect("repository native binary");
    }
    let original = std::env::var_os("PATH").expect("system PATH");
    std::env::set_var(
        "PATH",
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&original)))
            .expect("isolated PATH"),
    );
    let nested = fixture.root.join("nested");
    std::fs::create_dir(&nested).expect("nested selection");
    let opened = Workspace::open(nested.to_str().expect("UTF8 nested"), &fixture.database);
    if mode == "wrapper" {
        assert!(
            matches!(opened, Err(ref error) if error.code == "COMMAND_APPROVAL"),
            "automatic discovery must reject a resolved batch wrapper"
        );
        assert!(!marker.exists(), "discovery executed the untrusted wrapper");
    } else {
        assert_eq!(
            opened.expect("repository PATH binary must be skipped").root,
            fixture.workspace.root
        );
    }
}
#[cfg(windows)]
#[test]
fn workspace_discovery_rejects_wrappers_and_repository_path_binaries() {
    for mode in ["wrapper", "repository_native"] {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "workspace_discovery_helper",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("ASTRAFORGE_DISCOVERY_MODE", mode)
            .output()
            .expect("isolated discovery fixture");
        assert!(
            output.status.success(),
            "{mode}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
fn verify_integrity(database: &Database) {
    database
        .with(|connection| {
            let integrity: String =
                connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            assert_eq!(integrity, "ok");
            let violations: i64 = connection.query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_check",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(violations, 0);
            Ok(())
        })
        .expect("persistent integrity");
}
#[test]
fn rejecting_a_future_database_preserves_its_journal_mode() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("future.db");
    let connection = rusqlite::Connection::open(&path).expect("future database");
    connection
        .execute_batch("PRAGMA journal_mode=DELETE; PRAGMA user_version=999;")
        .expect("future schema");
    drop(connection);
    assert!(matches!(Database::open(&path), Err(error) if error.code == "DB_NEWER_SCHEMA"));
    let connection = rusqlite::Connection::open(&path).expect("inspect future database");
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal mode");
    assert_eq!(
        mode, "delete",
        "rejecting an unsupported database must not alter its persistent mode"
    );
}
#[cfg(windows)]
#[test]
fn windows_distinct_unicode_paths_can_share_a_patch_but_ascii_aliases_cannot() {
    let fixture = Fixture::new();
    let patch = fixture
        .patches
        .propose(
            &fixture.workspace,
            vec![
                fixture.proposal("K.txt", "latin", "latin updated"),
                fixture.proposal("K.txt", "kelvin", "kelvin updated"),
            ],
            "unicode paths",
        )
        .expect("distinct NTFS paths");
    fixture
        .patches
        .apply(&fixture.workspace, &patch.id)
        .expect("apply distinct paths");
    assert_eq!(
        fixture.workspace.read("K.txt").expect("latin").content,
        "latin updated"
    );
    assert_eq!(
        fixture.workspace.read("K.txt").expect("kelvin").content,
        "kelvin updated"
    );
    fixture
        .patches
        .revert(&fixture.workspace, &patch.id)
        .expect("revert distinct paths");
    let alias = fixture.patches.propose(
        &fixture.workspace,
        vec![
            PatchProposal {
                path: "K.txt".into(),
                content: Some("one".into()),
                expected_hash: Some(content_hash("latin")),
            },
            PatchProposal {
                path: "k.txt".into(),
                content: Some("two".into()),
                expected_hash: Some(content_hash("latin")),
            },
        ],
        "alias paths",
    );
    assert_eq!(
        alias
            .expect_err("alias must be rejected before mutation")
            .code,
        "PATCH_DUPLICATE"
    );
    fixture.verify_integrity();
}
#[test]
fn patch_sql_failures_preserve_recoverable_journals_and_file_contents() {
    for boundary in ["journal", "commit", "rollback"] {
        let fixture = Fixture::new();
        let patch = fixture
            .patches
            .propose(
                &fixture.workspace,
                vec![
                    fixture.proposal("first.txt", "first before", "first after"),
                    fixture.proposal("second.txt", "second before", "second after"),
                ],
                "database failure",
            )
            .expect("proposal");
        let condition = match boundary {
            "journal" => "NEW.status='applying'",
            "commit" => "NEW.status='applied'",
            _ => "NEW.status='applied' OR (NEW.status='proposed' AND OLD.status='applying')",
        };
        fixture.database.with(|connection| {
            connection.execute_batch(&format!("CREATE TRIGGER injected_failure BEFORE UPDATE OF status ON patch_sets WHEN {condition} BEGIN SELECT RAISE(ABORT,'injected persistence failure'); END;"))?;
            Ok(())
        }).expect("install failure injection");
        let error = fixture
            .patches
            .apply(&fixture.workspace, &patch.id)
            .expect_err("injected failure");
        assert_eq!(
            error.code,
            match boundary {
                "journal" => "DB_ERROR",
                "commit" => "PATCH_ROLLED_BACK",
                _ => "PATCH_RECOVERY_REQUIRED",
            }
        );
        assert_eq!(
            fixture
                .workspace
                .read("first.txt")
                .expect("first restored")
                .content,
            "first before"
        );
        assert_eq!(
            fixture
                .workspace
                .read("second.txt")
                .expect("second restored")
                .content,
            "second before"
        );
        assert_eq!(fixture.journal_count(), i64::from(boundary == "rollback"));
        fixture
            .database
            .with(|connection| {
                connection.execute_batch("DROP TRIGGER injected_failure;")?;
                Ok(())
            })
            .expect("remove failure injection");
        fixture
            .patches
            .recover(&fixture.workspace)
            .expect("retry durable recovery");
        assert_eq!(fixture.journal_count(), 0);
        assert_eq!(
            fixture
                .patches
                .list(&fixture.workspace)
                .expect("patch status")[0]
                .status,
            "proposed"
        );
        fixture.verify_integrity();
    }
}
#[test]
fn rollback_preserves_external_change_and_retains_recovery_until_resolved() {
    let fixture = Fixture::new();
    let patch = fixture
        .patches
        .propose(
            &fixture.workspace,
            vec![
                fixture.proposal("first.txt", "first before", "first after"),
                fixture.proposal("second.txt", "second before", "second after"),
            ],
            "external mutation",
        )
        .expect("proposal");
    let target = fixture.root.join("first.txt");
    fixture.database.with(|connection| {
        connection.create_scalar_function("external_mutation", 0, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS, move |_| {
            std::fs::write(&target, "external writer").map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;
            Ok(1i64)
        })?;
        connection.execute_batch("CREATE TRIGGER injected_failure BEFORE UPDATE OF status ON patch_sets WHEN NEW.status='applied' BEGIN SELECT external_mutation(); SELECT RAISE(ABORT,'injected after external write'); END;")?;
        Ok(())
    }).expect("install simultaneous write injection");
    assert_eq!(
        fixture
            .patches
            .apply(&fixture.workspace, &patch.id)
            .expect_err("conflict retained")
            .code,
        "PATCH_RECOVERY_REQUIRED"
    );
    assert_eq!(
        fixture
            .workspace
            .read("first.txt")
            .expect("external content")
            .content,
        "external writer"
    );
    assert_eq!(
        fixture
            .workspace
            .read("second.txt")
            .expect("other file restored")
            .content,
        "second before"
    );
    assert_eq!(fixture.journal_count(), 1);
    fixture
        .database
        .with(|connection| {
            connection.execute_batch("DROP TRIGGER injected_failure;")?;
            Ok(())
        })
        .expect("remove injection");
    assert_eq!(
        fixture
            .patches
            .recover(&fixture.workspace)
            .expect_err("external content still protected")
            .code,
        "PATCH_RECOVERY_REQUIRED"
    );
    std::fs::write(fixture.root.join("first.txt"), "first after")
        .expect("manual conflict resolution");
    fixture
        .patches
        .recover(&fixture.workspace)
        .expect("recover after resolution");
    assert_eq!(
        fixture
            .workspace
            .read("first.txt")
            .expect("original restored")
            .content,
        "first before"
    );
    assert_eq!(fixture.journal_count(), 0);
    fixture.verify_integrity();
}
#[test]
#[ignore = "Invoked in real subprocesses by patch_process_crashes_recover_committed_journals"]
fn patch_crash_helper() {
    let root = PathBuf::from(std::env::var_os("ASTRAFORGE_CRASH_ROOT").expect("isolated root"));
    let path =
        PathBuf::from(std::env::var_os("ASTRAFORGE_CRASH_DATABASE").expect("isolated database"));
    let patch_id = std::env::var("ASTRAFORGE_CRASH_PATCH").expect("patch id");
    let mode = std::env::var("ASTRAFORGE_CRASH_MODE").expect("crash mode");
    let database = Arc::new(Database::open(&path).expect("child database"));
    let workspace =
        Workspace::open(root.to_str().expect("UTF8 root"), &database).expect("child workspace");
    let patches = PatchService::new(database.clone());
    if !mode.starts_with("after_") {
        let status = match mode.as_str() {
            "journal_apply" => "applying",
            "commit_apply" => "applied",
            "journal_revert" => "reverting",
            "commit_revert" => "reverted",
            _ => panic!("unknown crash point"),
        };
        database.with(|connection| {
            connection.create_scalar_function("terminate_process", 0, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS, |_| -> rusqlite::Result<i64> { std::process::exit(73) })?;
            connection.execute_batch(&format!("CREATE TRIGGER crash_process BEFORE UPDATE OF status ON patch_sets WHEN NEW.status='{status}' BEGIN SELECT terminate_process(); END;"))?;
            Ok(())
        }).expect("child crash point");
    }
    if mode.ends_with("revert") {
        patches.revert(&workspace, &patch_id).expect("child revert");
    } else {
        patches.apply(&workspace, &patch_id).expect("child apply");
    }
    std::process::exit(73);
}
#[test]
fn patch_process_crashes_recover_committed_journals() {
    for mode in [
        "journal_apply",
        "commit_apply",
        "after_apply",
        "journal_revert",
        "commit_revert",
        "after_revert",
    ] {
        let directory = tempfile::tempdir().expect("crash fixture");
        let root = directory.path().join("repository");
        std::fs::create_dir(&root).expect("repository");
        initialize_git(&root);
        let path = directory.path().join("state.db");
        let patch_id = {
            let database = Arc::new(Database::open(&path).expect("parent database"));
            let workspace = Workspace::open(root.to_str().expect("UTF8 root"), &database)
                .expect("parent workspace");
            let patches = PatchService::new(database);
            let proposals = (0..4)
                .map(|number| {
                    let name = format!("file-{number}.txt");
                    std::fs::write(root.join(&name), "before").expect("original file");
                    PatchProposal {
                        path: name,
                        content: Some("after".into()),
                        expected_hash: Some(content_hash("before")),
                    }
                })
                .collect();
            let patch = patches
                .propose(&workspace, proposals, "actual process crash")
                .expect("proposal");
            if mode.ends_with("revert") {
                patches.apply(&workspace, &patch.id).expect("initial apply");
            }
            patch.id
        };
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["patch_crash_helper", "--exact", "--ignored", "--nocapture"])
            .env("ASTRAFORGE_CRASH_ROOT", &root)
            .env("ASTRAFORGE_CRASH_DATABASE", &path)
            .env("ASTRAFORGE_CRASH_PATCH", &patch_id)
            .env("ASTRAFORGE_CRASH_MODE", mode)
            .output()
            .expect("crashing child");
        assert_eq!(
            output.status.code(),
            Some(73),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let database = Arc::new(Database::open(&path).expect("database reopens after crash"));
        database
            .with(|connection| {
                connection.execute_batch("DROP TRIGGER IF EXISTS crash_process;")?;
                Ok(())
            })
            .expect("remove test-only trigger");
        let workspace = Workspace::open(root.to_str().expect("UTF8 root"), &database)
            .expect("workspace after crash");
        let patches = PatchService::new(database.clone());
        let before_recovery = database
            .with(|connection| {
                Ok(
                    connection.query_row("SELECT COUNT(*) FROM patch_journal", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .expect("durable journal");
        assert_eq!(
            before_recovery,
            i64::from(mode.starts_with("commit_")),
            "{mode}"
        );
        let recovered = patches.recover(&workspace).expect("crash recovery");
        assert_eq!(
            recovered.len(),
            usize::from(mode.starts_with("commit_")),
            "{mode}"
        );
        assert!(patches
            .recover(&workspace)
            .expect("idempotent recovery")
            .is_empty());
        let expected =
            if mode == "after_apply" || (mode.ends_with("revert") && mode != "after_revert") {
                "after"
            } else {
                "before"
            };
        for number in 0..4 {
            assert_eq!(
                workspace
                    .read(&format!("file-{number}.txt"))
                    .expect("recovered content")
                    .content,
                expected,
                "{mode}"
            );
        }
        let status = patches.list(&workspace).expect("recovered patch")[0]
            .status
            .clone();
        assert_eq!(
            status,
            if mode == "after_revert" {
                "reverted"
            } else if expected == "after" {
                "applied"
            } else {
                "proposed"
            }
        );
        verify_integrity(&database);
    }
}
#[test]
fn simultaneous_duplicate_patch_applications_have_one_winner() {
    let fixture = Fixture::new();
    let patch = fixture
        .patches
        .propose(
            &fixture.workspace,
            vec![fixture.proposal("shared.txt", "before", "after")],
            "concurrent apply",
        )
        .expect("proposal");
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            let patches = fixture.patches.clone();
            let workspace = fixture.workspace.clone();
            let id = patch.id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                patches.apply(&workspace, &id)
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker completed"))
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .all(|error| error.code == "PATCH_STATE"));
    assert_eq!(
        fixture
            .workspace
            .read("shared.txt")
            .expect("final content")
            .content,
        "after"
    );
    assert_eq!(fixture.journal_count(), 0);
    fixture.verify_integrity();
}
#[test]
fn simultaneous_optimistic_saves_never_lose_a_successful_update() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("shared.txt"), "initial").expect("shared file");
    let hash = content_hash("initial");
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|number| {
            let workspace = fixture.workspace.clone();
            let barrier = barrier.clone();
            let hash = hash.clone();
            std::thread::spawn(move || {
                barrier.wait();
                workspace.write("shared.txt", &format!("writer-{number}"), &hash)
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("save worker"))
        .collect();
    let saved: Vec<_> = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect();
    assert_eq!(saved.len(), 1);
    assert!(results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .all(|error| error.code == "STALE_FILE"));
    assert_eq!(
        fixture
            .workspace
            .read("shared.txt")
            .expect("saved content")
            .hash,
        saved[0].hash
    );
}
#[test]
fn files_from_one_to_five_hundred_mebibytes_are_bounded_before_reading() {
    let fixture = Fixture::new();
    for size in [1, 10, 50, 100, 500] {
        let name = format!("large-{size}.bin");
        let file = std::fs::File::create(fixture.root.join(&name)).expect("sparse fixture");
        file.set_len(size * 1024 * 1024).expect("set logical size");
        drop(file);
        let error = fixture
            .workspace
            .read(&name)
            .expect_err("binary or large file safely rejected");
        assert_eq!(
            error.code,
            if size == 1 {
                "BINARY_FILE"
            } else {
                "FILE_TOO_LARGE"
            }
        );
    }
    std::fs::write(fixture.root.join("text.txt"), "x".repeat(MAX_TEXT_BYTES))
        .expect("maximum text");
    assert_eq!(
        fixture
            .workspace
            .read("text.txt")
            .expect("maximum allowed file")
            .content
            .len(),
        MAX_TEXT_BYTES
    );
}
#[test]
fn concurrent_database_transactions_rollback_failures_without_orphans() {
    let fixture = Fixture::new();
    let workers: Vec<_> = (0..6)
        .map(|worker| {
            let database = fixture.database.clone();
            std::thread::spawn(move || {
                let mut committed = 0;
                for number in 0..40 {
                    let result = database.with(|connection| {
                        let transaction = connection.transaction()?;
                        transaction.execute(
                            "INSERT INTO settings(key,value) VALUES(?1,'value')",
                            [format!("{worker}:{number}")],
                        )?;
                        if number % 7 == 0 {
                            return Err(AppError::new("INJECTED_ABORT", "transaction aborted"));
                        }
                        transaction.commit()?;
                        Ok(())
                    });
                    if number % 7 == 0 {
                        assert_eq!(
                            result.expect_err("abort rolled back").code,
                            "INJECTED_ABORT"
                        );
                    } else {
                        result.expect("commit");
                        committed += 1;
                    }
                }
                committed
            })
        })
        .collect();
    let committed: i64 = workers
        .into_iter()
        .map(|worker| worker.join().expect("database worker"))
        .sum();
    fixture.database.with(|connection| {
        let actual: i64 = connection.query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))?;
        assert_eq!(actual, committed);
        assert!(connection.execute("INSERT INTO patch_sets(id,repository_id,status,source,created_at,changes) VALUES('orphan','missing','proposed','test',0,'[]')", []).is_err());
        Ok(())
    }).expect("transaction and foreign key assertions");
    fixture.verify_integrity();
}
proptest! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: Some(Box::new(FileFailurePersistence::WithSource("proptest-regressions"))), ..ProptestConfig::default() })]
    #[test]
    fn accepted_arbitrary_unicode_paths_stay_relative(input in ".{0,512}") {
        if let Ok(normalized) = normalize_path(&input) {
            prop_assert!(normalized.len() <= 4096);
            prop_assert!(!Path::new(&normalized).is_absolute());
            prop_assert!(Path::new(&normalized).components().all(|part| matches!(part, std::path::Component::Normal(_))));
            prop_assert_eq!(normalize_path(&normalized).expect("idempotent normalization"), normalized);
        }
    }
}

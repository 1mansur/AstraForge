use astraforge_core::database::Database;
use astraforge_core::git::GitService;
use astraforge_core::index::{adapter_for, IndexService};
use astraforge_core::process::{classify, CommandSpec, ProcessManager, ProcessSnapshot};
use astraforge_core::workspace::Workspace;
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
struct Fixture {
    db: Arc<Database>,
    workspace: Workspace,
    _temporary: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("temporary repository");
        let root = temporary.path().join("repository");
        std::fs::create_dir(&root).expect("create root");
        let output = Command::new("git")
            .args(["init", "-q"])
            .arg(&root)
            .output()
            .expect("git installed");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let db =
            Arc::new(Database::open(&temporary.path().join("state.sqlite")).expect("database"));
        let workspace =
            Workspace::open(root.to_str().expect("UTF-8 root"), &db).expect("open repository");
        Self {
            db,
            workspace,
            _temporary: temporary,
        }
    }
    fn write(&self, path: &str, content: &str) {
        let absolute = self.workspace.root.join(path);
        std::fs::create_dir_all(absolute.parent().expect("file parent"))
            .expect("create directories");
        std::fs::write(absolute, content).expect("write source");
    }
    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(&self.workspace.root)
            .args(args)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
fn spec(program: &str, args: &[&str], approved: bool) -> CommandSpec {
    CommandSpec {
        program: program.to_owned(),
        args: args.iter().map(|value| (*value).to_owned()).collect(),
        cwd: None,
        env: BTreeMap::new(),
        approved,
        is_test: false,
    }
}
fn await_exit(manager: &ProcessManager, id: &str) -> ProcessSnapshot {
    let started = Instant::now();
    loop {
        let snapshot = manager.poll(id, 0).expect("poll command");
        if !snapshot.running {
            return snapshot;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "command timed out"
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}
#[test]
fn extracts_ast_symbols_across_supported_languages() {
    for (path, source, expected) in [
        ("code.ts", "export interface User { name: string }\nexport function greet(user: User) { return user.name }", "greet"),
        ("code.tsx", "export function App() { return <main>Hello</main> }", "App"),
        ("code.js", "export class Worker { run() { return 1; } }", "Worker"),
        ("code.rs", "pub struct User { name: String }\npub fn greet() {}", "greet"),
        ("code.py", "class User:\n    def greet(self):\n        return 1\n", "greet"),
        ("code.json", "{\"name\":\"AstraForge\",\"nested\":{\"enabled\":true}}", "enabled"),
        ("code.md", "# Architecture\n\n[Engine](./engine.ts)\n", "Architecture"),
    ] {
        let parsed = adapter_for(path).expect("language adapter").parse(source).expect("parse");
        assert!(!parsed.has_errors, "{path}");
        assert!(parsed.symbols.iter().any(|symbol| symbol.name == expected), "{path}: {:?}", parsed.symbols);
    }
}
#[test]
fn malformed_source_preserves_partial_ast_and_reports_errors() {
    let parsed = adapter_for("bad.ts")
        .expect("adapter")
        .parse("function okay() {}\nfunction broken( {")
        .expect("error-tolerant parser");
    assert!(parsed.has_errors);
    assert!(parsed.symbols.iter().any(|symbol| symbol.name == "okay"));
}
#[test]
fn index_persists_search_and_updates_only_changed_files() {
    let fixture = Fixture::new();
    fixture.write(
        "src/a.ts",
        "import { greet } from './b';\nexport function run() { return greet(); }\n",
    );
    fixture.write("src/b.ts", "export function greet() { return 'hello'; }\n");
    fixture.write(".gitignore", "ignored/\n");
    fixture.write("ignored/secret.ts", "export const omitted = true;");
    let index = IndexService::new(fixture.db.clone()).expect("index");
    let stats = index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .expect("full index");
    assert_eq!(stats.files, 3);
    assert_eq!(
        index
            .search(&fixture.workspace, "greet", "symbol", 0, 100)
            .expect("symbols")
            .len(),
        1
    );
    assert!(index
        .search(&fixture.workspace, "omitted", "text", 0, 100)
        .expect("ignored search")
        .is_empty());
    assert!(index
        .graph(&fixture.workspace, "src/a.ts", "dependencies")
        .expect("graph")
        .iter()
        .any(|edge| edge.target == "src/b.ts"));
    assert_eq!(
        index
            .graph(&fixture.workspace, "src/b.ts", "dependents")
            .expect("dependents")
            .len(),
        1
    );
    assert!(!index
        .graph(&fixture.workspace, "greet", "callers")
        .expect("callers")
        .is_empty());
    assert_eq!(
        index
            .index(&fixture.workspace, &AtomicBool::new(false))
            .expect("unchanged index")
            .files,
        0
    );
    fixture.write("src/b.ts", "export function welcome() { return 'new'; }\n");
    let update = index
        .update(
            &fixture.workspace,
            &["src/b.ts".to_owned()],
            &AtomicBool::new(false),
        )
        .expect("incremental update");
    assert_eq!(update.files, 1);
    assert!(index
        .search(&fixture.workspace, "greet", "symbol", 0, 100)
        .expect("old symbol")
        .is_empty());
    drop(index);
    let reopened = IndexService::new(fixture.db.clone()).expect("reopened index");
    assert_eq!(
        reopened
            .search(&fixture.workspace, "welcome", "symbol", 0, 100)
            .expect("persisted symbol")
            .len(),
        1
    );
    std::fs::remove_file(fixture.workspace.root.join("src/b.ts")).expect("remove file");
    reopened
        .update(
            &fixture.workspace,
            &["src/b.ts".to_owned()],
            &AtomicBool::new(false),
        )
        .expect("deletion update");
    assert!(reopened
        .search(&fixture.workspace, "welcome", "symbol", 0, 100)
        .expect("removed symbol")
        .is_empty());
}
#[test]
fn graph_cycles_terminate_and_cancelled_index_preserves_previous_data() {
    let fixture = Fixture::new();
    fixture.write("a.ts", "import './b'; export const a = 1;");
    fixture.write("b.ts", "import './a'; export const b = 1;");
    let index = IndexService::new(fixture.db.clone()).expect("index");
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .expect("index");
    assert_eq!(
        index
            .graph(&fixture.workspace, "a.ts", "affected")
            .expect("cycle traversal")
            .len(),
        2
    );
    assert!(
        index
            .index(&fixture.workspace, &AtomicBool::new(true))
            .expect("cancel")
            .cancelled
    );
    assert_eq!(
        index
            .search(&fixture.workspace, ".ts", "filename", 0, 100)
            .expect("preserved")
            .len(),
        2
    );
}
#[test]
fn directory_changes_and_gitignore_are_reconciled_incrementally() {
    let fixture = Fixture::new();
    fixture.write("src/nested/a.ts", "export const retained = 1;");
    fixture.write(".gitignore", "ignored/\n");
    fixture.write("ignored/secret.ts", "export const secret = 1;");
    let index = IndexService::new(fixture.db.clone()).expect("index");
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .expect("index");
    index
        .update(
            &fixture.workspace,
            &["ignored/secret.ts".to_owned()],
            &AtomicBool::new(false),
        )
        .expect("ignored watcher event");
    assert!(index
        .search(&fixture.workspace, "secret", "symbol", 0, 100)
        .expect("ignored")
        .is_empty());
    std::fs::rename(
        fixture.workspace.root.join("src"),
        fixture.workspace.root.join("renamed"),
    )
    .expect("rename directory");
    index
        .update(
            &fixture.workspace,
            &["src".to_owned(), "renamed".to_owned()],
            &AtomicBool::new(false),
        )
        .expect("directory event");
    let hits = index
        .search(&fixture.workspace, "retained", "symbol", 0, 100)
        .expect("renamed symbol");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "renamed/nested/a.ts");
    fixture.write(".gitignore", "ignored/\nrenamed/\n");
    index
        .update(
            &fixture.workspace,
            &[".gitignore".to_owned()],
            &AtomicBool::new(false),
        )
        .expect("ignore changed");
    assert!(index
        .search(&fixture.workspace, "retained", "symbol", 0, 100)
        .expect("newly ignored")
        .is_empty());
}
#[test]
fn search_is_paginated_literal_and_regex_validated() {
    let fixture = Fixture::new();
    fixture.write(
        "a.txt",
        "literal % value\nliteral _ value\nliteral [ value\n",
    );
    let index = IndexService::new(fixture.db.clone()).expect("index");
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .expect("index");
    assert_eq!(
        index
            .search(&fixture.workspace, "%", "text", 0, 100)
            .expect("literal percent")
            .len(),
        1
    );
    assert_eq!(
        index
            .search(&fixture.workspace, "literal", "text", 1, 1)
            .expect("page")[0]
            .line,
        2
    );
    assert_eq!(
        index
            .search(&fixture.workspace, "literal [_%]", "regex", 0, 100)
            .expect("regex")
            .len(),
        2
    );
    assert!(index
        .search(&fixture.workspace, "[", "regex", 0, 100)
        .is_err());
    assert!(index
        .search(&fixture.workspace, "value", "unsupported", 0, 100)
        .is_err());
}
#[test]
fn git_actions_use_real_index_history_branches_and_literal_paths() {
    let fixture = Fixture::new();
    fixture.git(&["config", "user.name", "AstraForge Test"]);
    fixture.git(&["config", "user.email", "tests@example.invalid"]);
    fixture.write("hello.ts", "export const hello = 1;\n");
    fixture.write("--flag.txt", "literal path\n");
    GitService::action(&fixture.workspace, "stage", "hello.ts").expect("stage");
    GitService::action(&fixture.workspace, "stage", "--flag.txt").expect("literal stage");
    assert!(GitService::diff(&fixture.workspace, Some("hello.ts"), true)
        .expect("staged diff")
        .contains("export const"));
    GitService::action(&fixture.workspace, "commit", "Initial implementation").expect("commit");
    assert_eq!(
        GitService::history(&fixture.workspace, None)
            .expect("history")
            .as_array()
            .expect("commits")
            .len(),
        1
    );
    GitService::action(&fixture.workspace, "branch", "feature/engine").expect("branch");
    GitService::action(&fixture.workspace, "checkout", "feature/engine").expect("checkout");
    assert!(GitService::branches(&fixture.workspace)
        .expect("branches")
        .contains(&"feature/engine".to_owned()));
    fixture.write("hello.ts", "export const hello = 2;\n");
    assert_eq!(
        GitService::status(&fixture.workspace).expect("status")["entries"]
            .as_array()
            .expect("entries")
            .len(),
        1
    );
    assert!(
        GitService::diff(&fixture.workspace, Some("hello.ts"), false)
            .expect("diff")
            .contains("+export const hello = 2")
    );
    assert!(GitService::action(&fixture.workspace, "checkout", "--force").is_err());
    assert!(GitService::action(&fixture.workspace, "stage", "../outside").is_err());
}
#[test]
fn policy_rejects_argument_injection_env_bypass_and_unapproved_shells() {
    assert_eq!(
        classify(&spec("git", &["status", "--short"], false)).level,
        "SAFE"
    );
    assert_eq!(
        classify(&spec("git", &["-c", "alias.pwn=!evil", "pwn"], false)).level,
        "CAUTION"
    );
    assert_eq!(
        classify(&spec("git", &["diff", "--output=outside"], false)).level,
        "CAUTION"
    );
    assert_eq!(
        classify(&spec("./git", &["status"], false)).level,
        "CAUTION"
    );
    assert_eq!(
        classify(&spec("pwsh", &["-Command", "Write-Output x"], false)).level,
        "DANGEROUS"
    );
    assert_eq!(classify(&spec("diskpart", &[], true)).level, "BLOCKED");
    let mut environment = spec("git", &["status"], false);
    environment
        .env
        .insert("GIT_CONFIG_COUNT".to_owned(), "1".to_owned());
    assert_eq!(classify(&environment).level, "CAUTION");
    let fixture = Fixture::new();
    let manager = ProcessManager::new(fixture.db.clone());
    assert_eq!(
        manager
            .start(
                &fixture.workspace,
                spec("cmd", &["/c", "echo denied"], false)
            )
            .expect_err("approval required")
            .code,
        "COMMAND_APPROVAL"
    );
}
#[test]
fn process_streams_real_output_and_persists_completion() {
    let fixture = Fixture::new();
    let manager = ProcessManager::new(fixture.db.clone());
    let id = manager
        .start(
            &fixture.workspace,
            spec("git", &["status", "--short"], false),
        )
        .expect("safe git command");
    let done = await_exit(&manager, &id);
    assert_eq!(done.exit_code, Some(0));
    drop(manager);
    let reopened = ProcessManager::new(fixture.db.clone());
    let recovered = reopened.poll(&id, 0).expect("persisted command");
    assert!(!recovered.running);
    assert_eq!(recovered.exit_code, Some(0));
}
fn helper_spec(mode: &str) -> CommandSpec {
    let executable = std::env::current_exe().expect("test executable");
    let mut command = spec(
        executable.to_str().expect("UTF-8 executable"),
        &["--ignored", "--exact", "process_helper", "--nocapture"],
        true,
    );
    command
        .env
        .insert("ASTRAFORGE_PROCESS_HELPER".to_owned(), mode.to_owned());
    command
}
#[test]
fn process_output_is_bounded_and_stdin_is_real() {
    let fixture = Fixture::new();
    let manager = ProcessManager::new(fixture.db.clone());
    let id = manager
        .start(&fixture.workspace, helper_spec("flood"))
        .expect("start flood");
    let done = await_exit(&manager, &id);
    assert_eq!(done.exit_code, Some(0));
    assert!(done.truncated);
    assert!(
        done.chunks
            .iter()
            .map(|chunk| chunk.text.len())
            .sum::<usize>()
            <= 136 * 1024
    );
    let id = manager
        .start(&fixture.workspace, helper_spec("stdin"))
        .expect("start stdin helper");
    manager.input(&id, "real input\n").expect("write stdin");
    let done = await_exit(&manager, &id);
    assert_eq!(done.exit_code, Some(0));
    assert!(done
        .chunks
        .iter()
        .any(|chunk| chunk.text.contains("received: real input")));
}
#[test]
fn process_can_be_cancelled() {
    let fixture = Fixture::new();
    let manager = ProcessManager::new(fixture.db.clone());
    let id = manager
        .start(&fixture.workspace, helper_spec("sleep"))
        .expect("start sleep");
    manager.stop(&id).expect("cancel");
    let done = await_exit(&manager, &id);
    assert_ne!(done.exit_code, Some(0));
    assert!(!done.running);
}
#[test]
fn stopping_a_process_terminates_its_descendants() {
    let fixture = Fixture::new();
    let manager = ProcessManager::new(fixture.db.clone());
    let marker = fixture.workspace.root.join("escaped-child.txt");
    let mut command = helper_spec("tree");
    command.env.insert(
        "ASTRAFORGE_CHILD_MARKER".to_owned(),
        marker.to_string_lossy().to_string(),
    );
    let id = manager
        .start(&fixture.workspace, command)
        .expect("start process tree");
    let started = Instant::now();
    loop {
        let snapshot = manager.poll(&id, 0).expect("poll tree");
        if snapshot
            .chunks
            .iter()
            .any(|chunk| chunk.text.contains("tree-ready"))
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "tree did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    manager.stop(&id).expect("kill tree");
    await_exit(&manager, &id);
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        !marker.exists(),
        "a descendant escaped process cancellation"
    );
}
#[test]
#[ignore = "Subprocess helper invoked only by process integration tests"]
fn process_helper() {
    match std::env::var("ASTRAFORGE_PROCESS_HELPER")
        .expect("helper mode")
        .as_str()
    {
        "flood" => {
            let line = "x".repeat(1024);
            let mut output = std::io::stdout().lock();
            for _ in 0..4096 {
                writeln!(output, "{line}").expect("write flood");
            }
        }
        "stdin" => {
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .expect("read stdin");
            println!("received: {}", line.trim());
        }
        "sleep" => std::thread::sleep(Duration::from_secs(120)),
        "tree" => {
            let mut child = Command::new(std::env::current_exe().expect("test binary"))
                .args(["--ignored", "--exact", "process_helper", "--nocapture"])
                .env("ASTRAFORGE_PROCESS_HELPER", "marker")
                .spawn()
                .expect("spawn child");
            println!("tree-ready");
            std::io::stdout().flush().expect("flush readiness");
            std::thread::sleep(Duration::from_secs(120));
            child.wait().expect("wait child");
        }
        "marker" => {
            std::thread::sleep(Duration::from_millis(1000));
            std::fs::write(
                std::env::var("ASTRAFORGE_CHILD_MARKER").expect("marker path"),
                "escaped",
            )
            .expect("write marker");
        }
        mode => panic!("Unexpected helper mode {mode}"),
    }
}
proptest! {
    #[test]
    fn untrusted_arguments_never_make_shell_safe(argument in ".{0,200}") {
        prop_assert_ne!(classify(&spec("cmd", &[&argument], false)).level, "SAFE");
        prop_assert_ne!(classify(&spec("python", &[&argument], false)).level, "SAFE");
    }
    #[test]
    fn parser_handles_arbitrary_unicode_without_panicking(source in ".{0,500}") {
        let result = adapter_for("source.ts").expect("adapter").parse(&source);
        prop_assert!(result.is_ok());
    }
}
#[test]
fn trigram_search_is_literal_transactional_and_rebuildable() {
    let fixture = Fixture::new();
    fixture.write("a.txt", "literal a\"b value\nПривет Мир\nold-token\n");
    let index = IndexService::new(fixture.db.clone()).expect("index");
    index
        .index(&fixture.workspace, &AtomicBool::new(false))
        .expect("index");
    assert_eq!(
        index
            .search(&fixture.workspace, "a\"b", "text", 0, 10)
            .expect("quote literal")
            .len(),
        1
    );
    assert_eq!(
        index
            .search(&fixture.workspace, "Привет", "text", 0, 10)
            .expect("unicode literal")
            .len(),
        1
    );
    assert!(index
        .search(&fixture.workspace, "привет", "text", 0, 10)
        .expect("case sensitive")
        .is_empty());
    fixture.write("a.txt", "new-token\n");
    index
        .update(
            &fixture.workspace,
            &["a.txt".to_owned()],
            &AtomicBool::new(false),
        )
        .expect("update");
    assert!(index
        .search(&fixture.workspace, "old-token", "text", 0, 10)
        .expect("stale removed")
        .is_empty());
    assert_eq!(
        index
            .search(&fixture.workspace, "new-token", "text", 0, 10)
            .expect("new token")
            .len(),
        1
    );
    fixture
        .db
        .with(|connection| {
            connection.execute(
                "INSERT INTO index_text(index_text,rank) VALUES('integrity-check',1)",
                [],
            )?;
            connection.execute("DROP TABLE index_text", [])?;
            Ok(())
        })
        .expect("validate and remove search projection");
    drop(index);
    let reopened = IndexService::new(fixture.db.clone()).expect("rebuild text projection");
    assert_eq!(
        reopened
            .search(&fixture.workspace, "new-token", "text", 0, 10)
            .expect("rebuilt")
            .len(),
        1
    );
    std::fs::remove_file(fixture.workspace.root.join("a.txt")).expect("remove source");
    reopened
        .index(&fixture.workspace, &AtomicBool::new(false))
        .expect("full prune");
    assert!(reopened
        .search(&fixture.workspace, "new-token", "text", 0, 10)
        .expect("pruned projection")
        .is_empty());
}
